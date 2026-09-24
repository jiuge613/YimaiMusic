//! LX 兼容音源（自定义音源脚本 / HTTP 接口音源）
//!
//! 设计取舍：**不内嵌 JS 运行时**。业界（lx-music-desktop）的音源脚本跑在
//! 沙箱里、通过 `globalThis.lx` 与宿主交互；其中占绝大多数的是「HTTP 接口
//! 型」脚本——脚本本身只做"拼 query 参数 → GET 接口 → 从 JSON 里取直链"，
//! 不含加密运算。对这类脚本，Yimai 在导入时**静态解析出契约**
//! （API_BASE、平台/音质声明），播放取链由本模块用 ureq 原生完成，效果与
//! 执行脚本一致而无需沙箱。
//!
//! 支持的接口协议（与 lx-online-api 系部署一致）：
//! - `GET {base}/url.php?source=&id=&quality=&[extra=]` → `{code:0,data:{url}}`
//! - `GET {base}/lyric.php?source=&id=` → `{code:0,data:{lyric,tlyric,…}}`
//! - `GET {base}/pic.php?source=&id=`   → `{code:0,data:{url}}`
//! - `GET {base}/platforms.php`（可选）→ `{code:0,data:{platforms:[…]}}`
//!
//! 含加密逻辑、依赖 crypto/zlib 的脚本无法静态执行，导入时会给出明确报错。

use std::time::Duration;

use regex::Regex;

use crate::models::{LxPlatform, LxSearchSong};

const TIMEOUT_CONNECT: Duration = Duration::from_secs(10);
const TIMEOUT_READ: Duration = Duration::from_secs(15);

const UA: &str = "Yimai/0.1 (LX-compatible source client)";

/// 已知平台代码 → 中文名（脚本/platforms.php 未提供名字时兜底）
const PLATFORM_NAMES: &[(&str, &str)] = &[
    ("wy", "网易云"),
    ("tx", "QQ"),
    ("kw", "酷我"),
    ("kg", "酷狗"),
    ("mg", "咪咕"),
    ("joox", "JOOX"),
    ("xm", "小咪"),
    ("bd", "百度"),
];

fn platform_name(code: &str) -> String {
    PLATFORM_NAMES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| code.to_string())
}

// ---------- 脚本解析 ----------

/// 解析结果：从脚本文本中静态提取的契约
#[derive(Debug)]
pub struct ParsedScript {
    pub name: String,
    pub base_url: String,
    pub platforms: Vec<LxPlatform>,
}

/// 解析 LX 音源脚本文本，提取 API_BASE 与平台/音质契约。
///
/// 校验链（每步失败都返回可操作的中文错误）：
/// 1. 必须引用 `globalThis.lx`（否则根本不是 LX 音源脚本）；
/// 2. 必须处理 `musicUrl` 动作（只做歌词/封面的脚本对本播放器无意义）；
/// 3. 必须能定位取链接口基址（API_BASE 常量或脚本内首个 https 字面量）。
pub fn parse_script(text: &str) -> Result<ParsedScript, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("脚本是空的".into());
    }
    if !trimmed.contains("globalThis.lx") && !trimmed.contains("globalThis[\"lx\"]") {
        return Err(
            "这不是 LX 音源脚本（未引用 globalThis.lx）。请确认选择的是音源脚本文件（如 source.js）"
                .into(),
        );
    }
    if !trimmed.contains("musicUrl") {
        return Err("脚本未声明 musicUrl 动作，无法用于取链播放".into());
    }

    let base = extract_base(trimmed)
        .ok_or_else(|| {
            "无法从脚本中定位取链接口地址（API_BASE）。该脚本可能内置加密运算或非常规接口协议，\
             当前版本仅支持 HTTP 接口型音源脚本（如 lx-online-api 部署）"
                .to_string()
        })?;

    // 脚本名：@name 元数据 → 取链域名。脚本内中文名多为 \uXXXX 转义写法，需解码
    let name = extract_script_name(trimmed)
        .map(|n| decode_js_escapes(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{} 音源", host_of(&base)));
    let platforms = extract_platforms(trimmed);

    Ok(ParsedScript { name, base_url: base, platforms })
}

/// 提取 API_BASE：优先 `const API_BASE = '…'`（兼容常见变量名与引号风格），
/// 回退到脚本内第一个出现的 https(s):// 域名字面量。
fn extract_base(text: &str) -> Option<String> {
    let re = Regex::new(
        r#"(?i)(?:const|let|var)\s+(?:API_BASE|API_HOST|BASE_URL|BASEURL|APIBASE|API_URL|apiBase|baseUrl)\s*=\s*['"]([^'"]+)['"]"#,
    )
    .ok()?;
    if let Some(cap) = re.captures(text) {
        let candidate = normalize_base(&cap[1]);
        if candidate.is_some() {
            return candidate;
        }
    }
    // 回退：脚本里第一个 http(s) 字面量（排除明显是文档/说明的链接）
    let re_url = Regex::new(r#"['"](https?://[^'"\s]+)['"]"#).ok()?;
    for cap in re_url.captures_iter(text) {
        let candidate = normalize_base(&cap[1]);
        if candidate.is_some() {
            return candidate;
        }
    }
    None
}

/// 归一化基址：去尾斜杠、去查询串；必须 http(s) 且含主机名。
fn normalize_base(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_string();
    if s.starts_with("http://") || s.starts_with("https://") {
        if let Some(pos) = s.find('?') {
            s.truncate(pos);
        }
        while s.ends_with('/') {
            s.pop();
        }
        let host_ok = s
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .map(|h| h.contains('.'))
            .unwrap_or(false);
        if host_ok {
            return Some(s);
        }
    }
    None
}

/// 提取脚本头部 `@name` 元数据
fn extract_script_name(text: &str) -> Option<String> {
    let re = Regex::new(r"@name\s+([^\r\n*]+)").ok()?;
    re.captures(text)
        .map(|c| c[1].trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 提取 sources 声明里的平台与音质：
/// 形如 `wy: { name: '网易云音乐', qualitys: ['128k', '320k', 'flac'] }`
/// （兼容双引号、无 name、`qualities`/`qualitys` 两种拼写、跨行数组）
fn extract_platforms(text: &str) -> Vec<LxPlatform> {
    let re = Regex::new(
        r#"(?m)^\s*(wy|tx|kw|kg|mg|joox|xm|bd)\s*:\s*\{([^{}]*)\}"#,
    )
    .expect("platform regex");
    let re_name = Regex::new(r#"(?i)\bname\s*:\s*['"]([^'"]*)['"]"#).expect("name regex");
    // 注意：既支持官方拼写 qualitys 也支持误写 qualities（qualit + y|ie + s）
    let re_quals =
        Regex::new(r#"(?i)\bqualit(?:y|ie)s?\s*:\s*\[([^\]]*)\]"#).expect("qualities regex");
    let re_actions =
        Regex::new(r#"(?i)\bactions\s*:\s*\[([^\]]*)\]"#).expect("actions regex");

    let mut out: Vec<LxPlatform> = Vec::new();
    for cap in re.captures_iter(text) {
        let code = cap[1].to_string();
        let body = &cap[2];
        let name = re_name
            .captures(body)
            .map(|c| decode_js_escapes(c[1].trim()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| platform_name(&code));
        let qualitys = re_quals.captures(body).map(|c| split_list(&c[1]));
        let actions = re_actions
            .captures(body)
            .map(|c| split_list(&c[1]))
            .unwrap_or_else(|| vec!["musicUrl".into()]);
        let qualitys = qualitys.unwrap_or_default();
        out.push(LxPlatform { code, name, actions, qualitys });
    }
    out
}

/// 解码 JS 字符串转义：`\uXXXX`（含大写十六进制）→ Unicode 字符。
/// 音源脚本里中文常用 `'\u7f51\u6613...'` 写法，不解码列表里会显示原始转义序列。
fn decode_js_escapes(s: &str) -> String {
    if !s.contains("\\u") {
        return s.to_string();
    }
    let re = Regex::new(r"\\u\{?([0-9a-fA-F]{4})\}?").expect("unicode escape regex");
    re.replace_all(s, |caps: &regex::Captures| {
        u32::from_str_radix(&caps[1], 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string())
            .unwrap_or_else(|| caps[0].to_string())
    })
    .into_owned()
}

/// 拆 `['128k', "320k", flac]` 这类 JS 数组字面量为干净字符串列表
fn split_list(body: &str) -> Vec<String> {
    body.split(',')
        .map(|seg| {
            seg.trim()
                .trim_matches(|c| c == '\'' || c == '"' || c == ' ')
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

// ---------- HTTP ----------

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT_CONNECT)
        .timeout_read(TIMEOUT_READ)
        .build()
}

/// 把网络错误翻译成带场景的中文提示（超时 / DNS / 状态码）。
/// ureq 的 TransportKind Display 为小写（"dns error: …" / "io error: …timed out…"），
/// 统一小写后匹配，避免大小写假设。
fn http_err(context: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("{context}: 服务器返回 HTTP {code}"),
        ureq::Error::Transport(t) => {
            let kind = t.kind().to_string().to_lowercase();
            if kind.starts_with("dns") {
                format!("{context}: 域名无法解析，请检查地址拼写与网络连接")
            } else if kind.contains("timed out") || kind.contains("timeout") {
                format!(
                    "{context}: 请求超时（{}s 连接 / {}s 读取），服务器无响应或网络不通",
                    TIMEOUT_CONNECT.as_secs(),
                    TIMEOUT_READ.as_secs()
                )
            } else if kind.starts_with("url") {
                format!("{context}: 地址格式无效")
            } else {
                format!("{context}: 网络错误（{kind}）")
            }
        }
    }
}

/// 统一的 JSON GET：非 2xx / 非正文都会给出场景化错误
fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let resp = agent()
        .get(url)
        .set("User-Agent", UA)
        .call()
        .map_err(|e| http_err("请求失败", e))?;
    resp.into_json::<serde_json::Value>()
        .map_err(|e| format!("响应不是合法 JSON: {e}"))
}

/// LX 接口约定：`{code:0, data:…}` 为成功；否则取 `msg` 作为错误
fn unwrap_lx_envelope(v: serde_json::Value, what: &str) -> Result<serde_json::Value, String> {
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = v
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("未知错误");
        return Err(format!("{what}失败: {msg}"));
    }
    v.get("data")
        .cloned()
        .ok_or_else(|| format!("{what}失败: 响应缺少 data 字段"))
}

// ---------- 探测（添加网络源时校验） ----------

/// 探测结果
pub struct ProbeOutcome {
    pub name: String,
    pub base_url: String,
    pub platforms: Vec<LxPlatform>,
    /// 命中的探测方式（UI 展示 / 错误提示用）
    pub via: String,
    /// 脚本订阅时携带的脚本文本（入库保留原文，避免二次拉取）
    pub origin: String,
}

/// 添加网络音源：拉取并校验。
///
/// 三段式探测（任一通过即接受，越靠前信息越全）：
/// 1. URL 以 .js 结尾 → 按音源脚本订阅拉取 + 静态解析；
/// 2. `{base}/platforms.php` → 平台契约（最强校验）；
/// 3. `{base}/url.php` 空参试探 → 只要返回 LX 风格 JSON 即认为接口存在（弱校验）。
pub fn probe_source(url: &str) -> Result<ProbeOutcome, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("地址不能为空".into());
    }
    // 订阅脚本：直接拉取解析（原文随结果带回，调用方入库不再二次拉取）
    let lower = trimmed.split('?').next().unwrap_or("").to_lowercase();
    if lower.ends_with(".js") {
        let text = http_get_text(trimmed)?;
        let parsed = parse_script(&text).map_err(|e| format!("订阅脚本校验失败: {e}"))?;
        return Ok(ProbeOutcome {
            name: parsed.name,
            base_url: parsed.base_url,
            platforms: parsed.platforms,
            via: "音源脚本订阅".into(),
            origin: text,
        });
    }

    let base = normalize_base(trimmed)
        .ok_or("地址格式无效：需要 http(s):// 开头的完整地址（示例 https://music.example.com）")?;

    // 1) platforms.php
    match get_json(&format!("{base}/platforms.php")) {
        Ok(v) => {
            if let Ok(data) = unwrap_lx_envelope(v, "平台列表") {
                let platforms = parse_platforms_php(&data);
                if !platforms.is_empty() {
                    return Ok(ProbeOutcome {
                        name: host_of(&base),
                        base_url: base,
                        platforms,
                        via: "platforms.php".into(),
                        origin: String::new(),
                    });
                }
            }
        }
        Err(e) => eprintln!("[lxsource] platforms.php 探测: {e}"),
    }

    // 2) url.php 弱探测（参数故意给空 id：只要接口存在，无论业务成败都返回 JSON）
    match get_json(&format!("{base}/url.php?source=wy&id=0&quality=128k")) {
        Ok(v) => {
            if v.get("code").is_some() {
                return Ok(ProbeOutcome {
                    name: host_of(&base),
                    base_url: base,
                    platforms: vec![],
                    via: "url.php 接口探测".into(),
                    origin: String::new(),
                });
            }
            Err("该地址可访问但不是音源接口（url.php 响应缺少 code 字段）。\
                 请填写音源站根地址（如 https://music.example.com）或音源脚本链接"
                .into())
        }
        Err(e) => Err(format!(
            "音源校验失败：{e}。已尝试 platforms.php 与 url.php，均不可达；\
             请确认地址正确、服务器在线，且为 lx-online-api 类音源站"
        )),
    }
}

fn http_get_text(url: &str) -> Result<String, String> {
    let resp = agent()
        .get(url)
        .set("User-Agent", UA)
        .call()
        .map_err(|e| http_err("拉取失败", e))?;
    resp.into_string()
        .map_err(|e| format!("读取响应失败: {e}"))
}

/// platforms.php 的 data.platforms[] → 契约（该部署字段名为 qualities）
fn parse_platforms_php(data: &serde_json::Value) -> Vec<LxPlatform> {
    let empty = Vec::new();
    let arr = data
        .get("platforms")
        .and_then(|p| p.as_array())
        .unwrap_or(&empty);
    arr.iter()
        .filter_map(|p| {
            let code = p.get("code")?.as_str()?.to_string();
            let name = p
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let qualitys: Vec<String> = p
                .get("qualities")
                .or_else(|| p.get("qualitys"))
                .and_then(|q| q.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let actions: Vec<String> = p
                .get("actions")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_else(|| vec!["musicUrl".into()]);
            Some(LxPlatform {
                code: code.clone(),
                name: if name.is_empty() { platform_name(&code) } else { name },
                actions,
                qualitys,
            })
        })
        .collect()
}

fn host_of(base: &str) -> String {
    base.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(base)
        .to_string()
}

// ---------- 取链 ----------

/// 取音频直链：`GET {base}/url.php?source&id&quality[&extra]` → data.url。
/// extra 为可选的扩展上下文（酷狗 hash / QQ media_mid 等），原样透传。
pub fn music_url(
    base: &str,
    source: &str,
    id: &str,
    quality: &str,
    extra: Option<&str>,
) -> Result<String, String> {
    if id.is_empty() || id == "0" {
        return Err("歌曲 ID 无效，无法取链".into());
    }
    let mut url = format!("{base}/url.php?source={source}&id={id}&quality={quality}");
    if let Some(ex) = extra {
        if !ex.is_empty() {
            url.push_str("&extra=");
            url.push_str(&percent_encode(ex));
        }
    }
    let data = unwrap_lx_envelope(get_json(&url)?, "取链")?;
    let link = data
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    if !link.starts_with("http://") && !link.starts_with("https://") {
        return Err("取链失败: 接口返回的直链无效".into());
    }
    Ok(link)
}

/// 极简 percent-encode（extra 是 JSON 文本，含 {}"/ 等需编码字符）
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------- 搜索（排行榜） ----------

/// 音源搜索：`GET {base}/search.php?source=&keyword=&limit=&page=`
///
/// 并非所有部署都提供搜索接口（LX 脚本常见契约只有 musicUrl / lyric / pic），
/// 调用方需处理"接口不存在"的失败并回退到内置平台搜索。
/// 字段命名在各部署间差异较大，这里做宽松解析：任一首缺 id 或标题即丢弃。
pub fn search_songs(
    base: &str,
    source: &str,
    keyword: &str,
    limit: i64,
) -> Result<Vec<LxSearchSong>, String> {
    let url = format!(
        "{base}/search.php?source={source}&keyword={}&limit={limit}&page=1",
        percent_encode(keyword)
    );
    let data = unwrap_lx_envelope(get_json(&url)?, "搜索")?;
    let empty = Vec::new();
    let arr = data
        .get("list")
        .and_then(|l| l.as_array())
        .or_else(|| data.get("songs").and_then(|l| l.as_array()))
        .or_else(|| data.get("data").and_then(|l| l.as_array()))
        .or_else(|| data.as_array())
        .unwrap_or(&empty);
    Ok(arr
        .iter()
        .map(parse_song)
        .filter(|s| !s.id.is_empty() && !s.title.is_empty())
        .collect())
}

/// 从搜索结果的一个条目解析统一歌曲结构（字段别名兼容多套部署）
fn parse_song(v: &serde_json::Value) -> LxSearchSong {
    LxSearchSong {
        id: first_str(v, &["id", "songmid", "songId", "mid", "hash", "rid"])
            .unwrap_or_default(),
        title: first_str(v, &["name", "songname", "songName", "title"]).unwrap_or_default(),
        artist: artists_str(v),
        album: album_str(v),
        duration_ms: duration_ms_of(v),
        platform: String::new(),
        // 酷狗 hash / QQ media_mid：取链时可能需要原样透传
        extra: first_str(v, &["hash", "media_mid", "mediaMid", "strMediaMid"])
            .unwrap_or_default(),
    }
}

/// 取第一个存在且非空的字符串字段（数字/布尔按字符串化）
fn first_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(x) = v.get(*k) {
            match x {
                serde_json::Value::String(s) => {
                    if !s.trim().is_empty() {
                        return Some(s.trim().to_string());
                    }
                }
                serde_json::Value::Number(n) => return Some(n.to_string()),
                _ => {}
            }
        }
    }
    None
}

/// 歌手：可能是字符串、`[{name}]` 或 `["a","b"]`
fn artists_str(v: &serde_json::Value) -> String {
    for k in ["artist", "artists", "singer", "singers", "author", "authors"] {
        if let Some(x) = v.get(k) {
            match x {
                serde_json::Value::String(s) => return s.trim().to_string(),
                serde_json::Value::Array(a) => {
                    let names: Vec<String> = a
                        .iter()
                        .filter_map(|e| match e {
                            serde_json::Value::String(s) => Some(s.trim().to_string()),
                            serde_json::Value::Object(_) => {
                                first_str(e, &["name", "title", "singer"])
                            }
                            _ => None,
                        })
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !names.is_empty() {
                        return names.join(" / ");
                    }
                }
                _ => {}
            }
        }
    }
    String::new()
}

/// 专辑：可能是字符串或 `{name}`
fn album_str(v: &serde_json::Value) -> String {
    for k in ["album", "albumName", "albumname", "album_title", "albumTitle"] {
        if let Some(x) = v.get(k) {
            match x {
                serde_json::Value::String(s) => return s.trim().to_string(),
                serde_json::Value::Object(_) => {
                    if let Some(n) = first_str(x, &["name", "title"]) {
                        return n;
                    }
                }
                _ => {}
            }
        }
    }
    String::new()
}

/// 时长：字段可能给秒也可能给毫秒（秒值恒 < 1000），统一成毫秒
fn duration_ms_of(v: &serde_json::Value) -> u64 {
    let raw = v
        .get("durationMs")
        .or_else(|| v.get("duration_ms"))
        .and_then(|x| x.as_u64())
        .map(|ms| (ms, true))
        .or_else(|| {
            v.get("duration")
                .or_else(|| v.get("interval"))
                .or_else(|| v.get("time"))
                .and_then(|x| x.as_u64())
                .map(|n| (n, false))
        });
    match raw {
        Some((n, true)) => n,
        // 小于 1000 视为秒（常规曲目 100–600 秒），否则按毫秒
        Some((n, false)) if n < 1000 => n * 1000,
        Some((n, false)) => n,
        None => 0,
    }
}

// ---------- 测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

const SAMPLE: &str = r#"
/*!
 * LX Online Source API (PHP shared-host build, ready to use)
 */
// API endpoint base = where url.php / lyric.php / pic.php live
const API_BASE = 'https://music.example.com'

const { EVENT_NAMES, request, on, send, utils } = globalThis.lx

const SOURCES = {
  wy: { name: '\u7f51\u6613\u4e91\u97f3\u4e50', qualitys: ['128k', '192k', '320k', 'flac', 'flac24bit', 'hires', 'atmos', 'master'] },
  tx: { name: 'QQ音乐', qualitys: ['128k', '192k', '320k', 'flac', 'flac24bit', 'hires', 'atmos', 'atmos_plus', 'master'] },
  kw: { name: '酷我音乐', qualitys: ['128k', '192k', '320k', 'flac', 'flac24bit'] },
}

on(EVENT_NAMES.request, ({ action, source, info }) => {
  switch (action) {
    case 'musicUrl':
      return httpGet('/url.php?' + qs + '&quality=' + (info.type || '320k')).then((data) => data.url)
    case 'lyric':
      return httpGet('/lyric.php?' + qs)
    case 'pic':
      return httpGet('/pic.php?' + qs).then((data) => data.url)
  }
})

send(EVENT_NAMES.inited, { status: true, openDevTools: false, sources })
"#;

#[test]
fn parses_sample_script() {
    let p = parse_script(SAMPLE).expect("parse ok");
    assert_eq!(p.base_url, "https://music.example.com");
    // 无 @name 头 → 回退「域名 音源」
    assert_eq!(p.name, "music.example.com 音源");
    assert_eq!(p.platforms.len(), 3);
    let wy = p.platforms.iter().find(|x| x.code == "wy").unwrap();
    // 脚本内中文为 \uXXXX 转义写法，必须解码为汉字
    assert_eq!(wy.name, "网易云音乐");
    assert!(wy.qualitys.contains(&"flac24bit".to_string()));
    assert!(wy.qualitys.contains(&"master".to_string()));
}

#[test]
fn decodes_js_unicode_escapes() {
    assert_eq!(decode_js_escapes(r"\u7f51\u6613"), "网易");
    assert_eq!(decode_js_escapes("plain"), "plain");
}

    #[test]
    fn rejects_non_lx_script() {
        let err = parse_script("console.log('hi')").unwrap_err();
        assert!(err.contains("不是 LX 音源脚本"), "{err}");
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_script("   ").is_err());
    }

    #[test]
    fn normalize_base_variants() {
        assert_eq!(
            normalize_base("https://music.example.com/"),
            Some("https://music.example.com".into())
        );
        assert_eq!(normalize_base("ftp://x"), None);
        assert_eq!(normalize_base("https://localhost"), None);
    }

    #[test]
    fn percent_encode_extra() {
        assert_eq!(percent_encode(r#"{"hash":"a/b"}"#), r#"%7B%22hash%22%3A%22a%2Fb%22%7D"#);
    }

    /// 搜索结果的字段别名兼容：songmid/songname/singer[{name}]/album{name}/interval(秒)
    #[test]
    fn parses_search_song_aliases() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"songmid":"003aBcD1","songname":"晴天","singer":[{"name":"周杰伦"}],
                "album":{"name":"叶惠美"},"interval":269}"#,
        )
        .unwrap();
        let s = parse_song(&v);
        assert_eq!(s.id, "003aBcD1");
        assert_eq!(s.title, "晴天");
        assert_eq!(s.artist, "周杰伦");
        assert_eq!(s.album, "叶惠美");
        // interval 给的是秒 → 转毫秒
        assert_eq!(s.duration_ms, 269_000);
    }

    /// 时长字段给出毫秒时不要二次换算
    #[test]
    fn parses_search_song_duration_ms() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"id":"7","name":"a","artist":"b","durationMs":269000}"#)
                .unwrap();
        assert_eq!(parse_song(&v).duration_ms, 269_000);
    }
}
