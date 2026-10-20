//! 向量语义召回层（M3 最后一块）—— 与全文层**同级**的可选层。
//!
//! # 这个模块在整条链路里的位置
//!
//! `text
//! 查询 → 嵌入服务(18191) 取查询向量 → 内存向量表暴力余弦 → 结构化过滤 → 精排 → 返回
//!          └──────────── 本模块 ────────────┘      └─ 回 store 取行 ┘    └─ rerank ┘
//! `
//!
//! # 五条不可让步的约束
//!
//! 1. **可选层。** 本模块的任何失败都只影响 `Stage::Vector`，
//!    绝不影响结构化基线与 BM25。服务连不上 / 超时 / 返回垃圾 / 建表失败，
//!    五种结局一律降级，**不冒泡成「检索整体失败」**。
//! 2. **状态不说谎。** 状态是「此刻的真实结局」：探活成功才 `Ready`，
//!    探活失败就是 `Failed(原因)`。**绝不因为「配置里写了地址」
//!    就假定它可用**——那正是本项目反复栽过的那类谎报。
//! 3. **懒建 + 可重建。** 启动时**不**建向量表：只做一次廉价探活。
//!    首次查询（或显式 `rebuild`）时才真正嵌入全部资产。
//!    理由是启动延迟与显存/CPU 占用都不该由一个「可选增强」来决定，
//!    而且服务在不在启动那一刻完全可能是相反的状态。
//! 4. **不加依赖。** 规模是 506 条级别，暴力余弦绰绰有余；
//!    为此引入一个 URL 解析 + TLS + 连接池的 HTTP 客户端（实测 ureq
//!    离线也要拖进 39 个 crate）与「不引入重依赖」的纪律直接冲突。
//!    所以这里用一个**最小 HTTP/1.1 客户端**（见 `rawhttp`），只支持
//!    `http://` 加定长或 chunked 响应 —— 嵌入服务两者都满足。
//! 5. **超时短、宁可降级。** 默认 2.5s，且**对整个请求**生效（连接 + 写入 +
//!    读取共用一条 deadline）。一个卡住的嵌入服务不得变成一次卡住的检索。
//!
//! # 为什么向量表在内存里
//!
//! 506 条 × 1024 维 × 4 字节 ≈ 2.0 MiB。落盘索引要处理增量、版本、损坏、
//! 并发读写与跨进程失效——这些复杂度换不来任何东西：进程起来后嵌入一次
//! 全量只要约 0.4 秒（实测 1405 texts/s）。**可重建**比**持久化**便宜得多。
//! 这与全文层刻意相反：BM25 的倒排重建成本高，所以它落盘；
//! 向量表重建成本低，所以它只在内存。两者各自的取舍都写在各自模块里。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::health::StageState;
use crate::store::{AssetStore, IndexSnapshot};

/// 默认嵌入服务地址。**只走本机回环**：嵌入服务是本地组件，不是外部依赖。
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:18191";
/// 覆盖嵌入服务地址的环境变量。
pub const ENV_ENDPOINT: &str = "AC_VECTOR_URL";
/// 覆盖超时（毫秒）的环境变量。
pub const ENV_TIMEOUT_MS: &str = "AC_VECTOR_TIMEOUT_MS";

/// 默认超时。**2.5s**：比一次正常交互的预算小得多，
/// 又足够覆盖「模型冷启动后第一次前向」的抖动。宁可降级，不要卡住查询。
pub const DEFAULT_TIMEOUT_MS: u64 = 2500;
/// 超时下限。再小就连本机回环的正常往返都盖不住，只会把一切打成「故障」。
pub const MIN_TIMEOUT_MS: u64 = 100;
/// 超时上限。超过这个数已经谈不上「不阻塞查询」了，钳住它免得配置写错变成事故。
pub const MAX_TIMEOUT_MS: u64 = 30_000;
/// 故障后的冷却时间：这段时间内不再重试，直接被判为不可用。
/// 没这一条，一个挂掉的服务会让**每一次查询**都白等一个超时。
pub const DEFAULT_COOLDOWN_MS: u64 = 5_000;
/// 维度上限。防止一个畸形响应对着一个 `vec![0.0; n]` 分配掉几个 G。
pub const MAX_DIM: usize = 8192;
/// 单次嵌入请求的批大小。
///
/// 实测（506 条真实资产文本，18191）：批 32 → 5046ms，批 128 → 4399ms，
/// 批 506 → 4160ms。批越大 round-trip 越少，但**边际收益在 128 附近就没了**，
/// 而一次请求越大，中途超时作废的成本越高。128 是这两者的折中。
pub const EMBED_BATCH: usize = 128;

/// 建表的**单请求**预算（毫秒），与查询用的 timeout 分开。
///
/// # 为什么必须分开
///
/// 查询嵌入是「一条短文本」，超时设 2.5s 是**宁可降级不要卡住查询**。
/// 而建表是「一次批量前向」：模型冷启动后第一次前向实测要 ~7s
/// （CUDA kernel autotune），拿 2.5s 去卡它，结果是**每次冷启动的首次建表
/// 都必然失败**——不是一个更快的结果，而是一个必然的错误。
/// 两者是不同的操作，就不该共用同一个预算。
///
/// 60s 足够 506 条在最慢的冷态下走完（实测冷态整表 ~12s），
/// 又不至于让一次畸形挂起拖住整个进程。
pub const DEFAULT_BUILD_TIMEOUT_MS: u64 = 60_000;
/// 响应体上限（字节）。同样是为了防畸形响应把内存吃穿。
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;


// 配置
// ─────────────────────────────────────────────────────────────────

/// 向量层配置。**全部可覆盖**，且覆盖来源只有一个：环境变量。
///
/// 为什么不做配置文件：本项目的配置面目前只有 DB 路径与端口，
/// 都走命令行/环境变量。为一个可选层单独引入一层配置文件格式，
/// 会让「这台机器上到底连的哪个地址」多出一个需要同时核对的地方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorConfig {
    /// 嵌入服务根地址，例如 `http://127.0.0.1:18191`。
    pub endpoint: String,
    /// **查询路径**单个 HTTP 请求的总预算。
    pub timeout: Duration,
    /// **建表路径**单个 HTTP 请求的总预算（见 DEFAULT_BUILD_TIMEOUT_MS）。
    pub build_timeout: Duration,
    /// 故障后多久允许重试。
    pub cooldown: Duration,
}

impl Default for VectorConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            build_timeout: Duration::from_millis(DEFAULT_BUILD_TIMEOUT_MS),
            cooldown: Duration::from_millis(DEFAULT_COOLDOWN_MS),
        }
    }
}

impl VectorConfig {
    /// 从环境变量读配置，缺省即默认值。
    ///
    /// **坏值不 panic、也不静默吞掉**：解析失败时回落默认值并把原因留在返回值里，
    /// 由装配方决定要不要打日志。一个拼错的 `AC_VECTOR_TIMEOUT_MS`
    /// 如果静默变成 0，向量层会「永远超时」而没人知道为什么。
    pub fn from_env() -> (Self, Vec<String>) {
        let mut cfg = Self::default();
        let mut warnings = Vec::new();

        if let Ok(raw) = std::env::var(ENV_ENDPOINT) {
            let t = raw.trim();
            if t.is_empty() {
                warnings.push(format!("{ENV_ENDPOINT} 为空，沿用默认 {DEFAULT_ENDPOINT}"));
            } else if !t.starts_with("http://") {
                // 只支持明文 http：嵌入服务在本机回环上，加 TLS 只是自找复杂度。
                warnings.push(format!(
                    "{ENV_ENDPOINT}={t} 不可用（只支持 http:// 前缀），沿用默认 {DEFAULT_ENDPOINT}"
                ));
            } else {
                cfg.endpoint = t.trim_end_matches('/').to_string();
            }
        }

        if let Ok(raw) = std::env::var(ENV_TIMEOUT_MS) {
            match raw.trim().parse::<u64>() {
                Ok(ms) if (MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&ms) => {
                    cfg.timeout = Duration::from_millis(ms);
                }
                Ok(ms) => warnings.push(format!(
                    "{ENV_TIMEOUT_MS}={ms} 越界（允许 {MIN_TIMEOUT_MS}..={MAX_TIMEOUT_MS}），沿用默认 {DEFAULT_TIMEOUT_MS}"
                )),
                Err(e) => warnings.push(format!(
                    "{ENV_TIMEOUT_MS}={raw} 不是整数（{e}），沿用默认 {DEFAULT_TIMEOUT_MS}"
                )),
            }
        }
        (cfg, warnings)
    }

    /// 探活地址。
    fn health_url(&self) -> String {
        format!("{}/health", self.endpoint.trim_end_matches('/'))
    }
    /// 嵌入地址。
    fn embed_url(&self) -> String {
        format!("{}/embed", self.endpoint.trim_end_matches('/'))
    }
}

// ─────────────────────────────────────────────────────────────────
// 最小 HTTP/1.1 客户端
// ─────────────────────────────────────────────────────────────────

/// 一次最小 HTTP 往返。
///
/// 刻意只做嵌入服务需要的那一小块：`http://`、GET/POST、
/// `Content-Length` 与 `chunked`、`Connection: close`。
/// **不做**重定向、连接复用、代理、TLS、HTTP/2 —— 每一样都要么需要额外依赖，
/// 要么对「连本机 18191」这个用途没有意义。
pub mod rawhttp {
    use super::*;

    /// 一次响应。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Response {
        pub status: u16,
        pub body: String,
    }

    /// 把 `http://host:port/...` 拆成 (host, port, path)。
    ///
    /// 不用 `url` crate：见模块文档，那要拖进一整棵 ICU 依赖树。
    /// 代价是这个解析器**只认它自己认得的形状**，认不出就明确报错，
    /// 绝不「猜一个大概率对的结果」。
    fn split_http_url(url: &str) -> Result<(String, u16, String), String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("只支持 http:// 地址，收到：{url}"))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        // userinfo 本服务不需要鉴权，出现即配置错误 —— 明确拒绝而不是悄悄丢掉。
        if authority.contains('@') {
            return Err(format!("不支持带 userinfo 的地址：{url}"));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().map_err(|_| format!("端口非法：{p}（地址 {url}）"))?;
                (h.to_string(), port)
            }
            None => (authority.to_string(), 80),
        };
        if host.is_empty() {
            return Err(format!("主机名为空：{url}"));
        }
        Ok((host, port, path.to_string()))
    }

    /// 发一个请求。整条链路共用 `deadline` —— 连接、写、读都算在里面。
    ///
    /// 这是本模块**最重要的一条实现决定**：如果只给 connect 设超时，
    /// 一个「连上了但永远不回包」的服务会让查询线程无限期挂住；
    /// 而如果只给 read 设超时，DNS/connect 慢同样能突破预算。
    /// 唯一能同时覆盖三者的做法是**一条绝对截止时间**。
    pub fn request(
        url: &str,
        method: &str,
        body: Option<&str>,
        deadline: Instant,
    ) -> Result<Response, String> {
        let (host, port, path) = split_http_url(url)?;
        let remaining = |tag: &str| -> Result<Duration, String> {
            let now = Instant::now();
            if now >= deadline {
                return Err(format!("{tag}超时（预算已耗尽）"));
            }
            Ok(deadline - now)
        };

        let addr = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| format!("解析 {host}:{port} 失败：{e}"))?
            .next()
            .ok_or_else(|| format!("{host}:{port} 没有可用地址"))?;

        let mut stream = TcpStream::connect_timeout(&addr, remaining("连接")?)
            .map_err(|e| format!("连接 {addr} 失败：{e}"))?;
        stream
            .set_read_timeout(Some(remaining("读取")?))
            .map_err(|e| format!("设置读超时失败：{e}"))?;
        stream
            .set_write_timeout(Some(remaining("写入")?))
            .map_err(|e| format!("设置写超时失败：{e}"))?;

        let payload = body.unwrap_or("");
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n"
        );
        if !payload.is_empty() {
            req.push_str("Content-Type: application/json\r\n");
            req.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
        req.push_str("\r\n");
        req.push_str(payload);

        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("发送请求失败：{e}"))?;
        stream.flush().map_err(|e| format!("刷新失败：{e}"))?;

        let mut buf: Vec<u8> = Vec::with_capacity(4096);
        let mut chunk = [0u8; 8192];
        loop {
            // 每一轮都重新看截止时间：读超时只保证「单次 read 不无限等」，
            // 挡不住一个每 100ms 吐 1 字节的服务。
            if Instant::now() >= deadline {
                return Err("读取响应超时（预算已耗尽）".to_string());
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > MAX_RESPONSE_BYTES {
                        return Err(format!("响应体超过 {MAX_RESPONSE_BYTES} 字节，已中止"));
                    }
                }
                Err(e) => {
                    // 操作系统层面的读超时会以 TimedOut/WouldBlock 报告，
                    // 而它的默认文案（Windows 上是「连接尝试失败」）会让
                    // 「服务卡住」看起来像「连不上」—— 两者的处置完全不同。
                    // 这里把它翻译成如实的「超时」。
                    let timed_out = matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    );
                    return Err(if timed_out {
                        "读取响应超时（服务未在预算内返回）".to_string()
                    } else {
                        format!("读取响应失败：{e}")
                    });
                }
            }
        }

        parse_response(&buf)
    }

    /// 解析一个 HTTP/1.1 响应。**只认这一种协议**，认不出就报错。
    fn parse_response(buf: &[u8]) -> Result<Response, String> {
        let sep = find_subslice(buf, b"\r\n\r\n")
            .ok_or_else(|| "响应头不完整（找不到空行）".to_string())?;
        let head = String::from_utf8_lossy(&buf[..sep]).to_string();
        let body_raw = &buf[sep + 4..];

        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("状态行非法：{status_line}"))?;

        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        for line in lines {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "content-length" {
                content_length = v.parse::<usize>().ok();
            } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }

        let body: Vec<u8> = if chunked {
            dechunk(body_raw)?
        } else if let Some(n) = content_length {
            if body_raw.len() < n {
                return Err(format!(
                    "响应体被截断：声明 {n} 字节，实际收到 {}",
                    body_raw.len()
                ));
            }
            body_raw[..n].to_vec()
        } else {
            // 没有长度也没有 chunked：读到连接关闭为止（我们已经 Connection: close）。
            body_raw.to_vec()
        };

        Ok(Response {
            status,
            body: String::from_utf8_lossy(&body).to_string(),
        })
    }

    fn dechunk(mut raw: &[u8]) -> Result<Vec<u8>, String> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            let sep = find_subslice(raw, b"\r\n")
                .ok_or_else(|| "chunked 响应缺少长度行".to_string())?;
            let line = String::from_utf8_lossy(&raw[..sep]).to_string();
            // 分块扩展（以分号开头）要丢掉，只取十六进制长度。
            let size_str = line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_str, 16)
                .map_err(|e| format!("chunked 长度行非法（{line}）：{e}"))?;
            raw = &raw[sep + 2..];
            if size == 0 {
                return Ok(out);
            }
            if raw.len() < size {
                return Err("chunked 分块被截断".to_string());
            }
            out.extend_from_slice(&raw[..size]);
            raw = &raw[size..];
            // 每个分块后面跟着一个 CRLF。
            if raw.len() >= 2 && &raw[..2] == b"\r\n" {
                raw = &raw[2..];
            }
        }
    }

    fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || hay.len() < needle.len() {
            return None;
        }
        hay.windows(needle.len()).position(|w| w == needle)
    }

    /// 把 URL 拆解结果暴露出来，让「只认 http」这条约束可断言。
    #[cfg(any(test, feature = "testing"))]
    pub fn split_for_test(url: &str) -> Result<(String, u16, String), String> {
        split_http_url(url)
    }

    /// 直接喂一段原始字节验证解析器（不经过网络）。
    #[cfg(any(test, feature = "testing"))]
    pub fn parse_for_test(buf: &[u8]) -> Result<Response, String> {
        parse_response(buf)
    }
}
// ─────────────────────────────────────────────────────────────────
// 嵌入客户端
// ─────────────────────────────────────────────────────────────────

/// 一次探活的结局：拿到的模型身份，或失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    /// 服务自报的模型名（可能为空串——不强求，但**不许编**）。
    pub model: String,
    /// 服务自报的维度（拿不到就是 `None`）。
    pub dim: Option<usize>,
}

/// 嵌入服务客户端。
///
/// **无状态**：每次请求新建一条 TCP 连接。理由是查询频率不高（一次查询一次嵌入），
/// 而连接池要处理过期、半开、重连三种边界——那是把「可选层的可靠性」
/// 建在一个比它自己还复杂的东西上。
pub struct EmbedderClient {
    cfg: VectorConfig,
}

impl EmbedderClient {
    pub fn new(cfg: VectorConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &VectorConfig {
        &self.cfg
    }

    /// 探活。成功返回服务自报的身份，失败返回**带原因**的错误。
    ///
    /// 判定口径刻意严格：HTTP 200 **且** JSON 里 status 为 ok 才算就绪。
    /// 只看「连得上」是不够的——服务可能起在端口上但模型还在加载
    /// （嵌入服务自身就会返回 loading），
    /// 那时把向量层报成 ready 就是谎报：查询送过去只会拿到空向量。
    pub fn probe(&self) -> Result<ServiceInfo, String> {
        let deadline = Instant::now() + self.cfg.timeout;
        let resp = rawhttp::request(&self.cfg.health_url(), "GET", None, deadline)?;
        if resp.status != 200 {
            return Err(format!("探活返回 HTTP {}", resp.status));
        }
        let v: serde_json::Value =
            serde_json::from_str(&resp.body).map_err(|e| format!("探活响应不是 JSON：{e}"))?;
        match v.get("status").and_then(|s| s.as_str()) {
            Some("ok") => {}
            other => {
                return Err(format!("服务未就绪（status={}）", other.unwrap_or("缺失")))
            }
        }
        let model = v
            .get("model")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let dim = v
            .get("dim")
            .and_then(|d| d.as_u64())
            .map(|d| d as usize)
            .filter(|d| *d > 0 && *d <= MAX_DIM);
        Ok(ServiceInfo { model, dim })
    }
    /// 把一批文本嵌入成向量。
    ///
    /// # 校验比解析更值得写
    ///
    /// 下面每一条校验都对应一种**会让检索静默出错**的畸形响应：
    /// 条数对不上（顺序张冠李戴）、维度不一致（余弦算出来是垃圾）、
    /// 空向量（除零）、NaN/Inf（比较全都假，排序结果取决于输入顺序）。
    /// 任一命中都返回 Err，由上层降级——**绝不「尽量用」**。
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_within(texts, self.cfg.timeout)
    }

    /// 用**建表预算**做一次批量嵌入（见 DEFAULT_BUILD_TIMEOUT_MS）。
    pub fn embed_for_build(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_within(texts, self.cfg.build_timeout.max(self.cfg.timeout))
    }

    fn embed_within(
        &self,
        texts: &[String],
        budget: Duration,
    ) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let payload = serde_json::json!({ "texts": texts }).to_string();
        let deadline = Instant::now() + budget;
        let resp = rawhttp::request(&self.cfg.embed_url(), "POST", Some(&payload), deadline)?;
        if resp.status != 200 {
            return Err(format!("嵌入返回 HTTP {}", resp.status));
        }
        let v: serde_json::Value =
            serde_json::from_str(&resp.body).map_err(|e| format!("嵌入响应不是 JSON：{e}"))?;
        // 服务在模型未就绪时会返回 200 加 error 字段 —— 必须当成失败。
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            return Err(format!("嵌入服务报错：{err}"));
        }
        let arr = v
            .get("vectors")
            .and_then(|a| a.as_array())
            .ok_or_else(|| "嵌入响应缺少 vectors 数组".to_string())?;
        if arr.len() != texts.len() {
            return Err(format!(
                "嵌入条数不符：请求 {} 条，返回 {} 条",
                texts.len(),
                arr.len()
            ));
        }
        let declared_dim = v.get("dim").and_then(|d| d.as_u64()).map(|d| d as usize);
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(arr.len());
        let mut seen_dim: Option<usize> = declared_dim.filter(|d| *d > 0 && *d <= MAX_DIM);
        for (i, item) in arr.iter().enumerate() {
            let list = item
                .as_array()
                .ok_or_else(|| format!("第 {i} 条向量不是数组"))?;
            if list.is_empty() {
                return Err(format!("第 {i} 条向量为空"));
            }
            if list.len() > MAX_DIM {
                return Err(format!("第 {i} 条向量维度 {} 超过上限 {MAX_DIM}", list.len()));
            }
            match seen_dim {
                None => seen_dim = Some(list.len()),
                Some(d) if d != list.len() => {
                    return Err(format!(
                        "第 {i} 条向量维度 {} 与前面的 {d} 不一致",
                        list.len()
                    ))
                }
                Some(_) => {}
            }
            let mut vec: Vec<f32> = Vec::with_capacity(list.len());
            for x in list {
                let f = x
                    .as_f64()
                    .ok_or_else(|| format!("第 {i} 条向量含非数值元素"))? as f32;
                if !f.is_finite() {
                    return Err(format!("第 {i} 条向量含 NaN/Inf"));
                }
                vec.push(f);
            }
            out.push(vec);
        }
        Ok(out)
    }
}

/// 余弦相似度。**两向量必须同维且非零**，否则返回 None。
///
/// 返回 Option 而不是 0.0：0.0 是一个合法的相似度（正交），
/// 用它表达「算不了」会让一条坏数据静默地排到最后，而不是被看见。
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na <= 0.0 || nb <= 0.0 {
        return None;
    }
    Some((dot / (na.sqrt() * nb.sqrt())) as f32)
}
// ─────────────────────────────────────────────────────────────────
// 向量表（内存，懒建，可重建）
// ─────────────────────────────────────────────────────────────────

/// 一条已嵌入的资产。
#[derive(Debug, Clone)]
struct VectorEntry {
    id: String,
    /// 建表时真正送去嵌入的文本（截断到 MAX_EMBED_CHARS）。
    text: String,
    vec: Vec<f32>,
}

/// 送进嵌入模型的单条文本上限（字符）。
///
/// Qwen3-Embedding 的上下文是 8192 token，远超资产文本的长度；
/// 这里截断纯粹是为了**不让一条畸形超长资产拖垮整批嵌入**——
/// 一批里有一条超限，整个请求都可能被服务端拒绝。
pub const MAX_EMBED_CHARS: usize = 2000;

/// 内存向量表。
///
/// # 为什么用读写锁而不是互斥锁
///
/// 建表之后，查询路径只读；建表/重建写。读多写极少，
/// 用读写锁让并发查询不必互相排队。写入期间读会被阻塞——
/// 这是**故意的**：读到一半换表会让同一次查询看到两代向量。
pub struct VectorIndex {
    client: EmbedderClient,
    entries: RwLock<Vec<VectorEntry>>,
    /// 建表输入的内容指纹。资产变了它就变，「是否过期」由它判定。
    fingerprint: RwLock<String>,
    /// 此刻的真实状态。
    state: RwLock<StageState>,
    /// 上次握手/建表成功时服务自报的身份，供健康检查如实报出。
    info: RwLock<Option<ServiceInfo>>,
    /// 冷却截止时刻：在此之前不再尝试，直接把失败返回。
    retry_after: Mutex<Option<Instant>>,
    /// 上次**成功**探活的时刻。见 SUCCESS_CACHE_MS。
    last_ok_at: Mutex<Option<Instant>>,
    /// 后台建表是否正在进行。
    building: std::sync::atomic::AtomicBool,
    /// **建表的失败原因**（与「服务可达性」正交的一件事）。
    ///
    /// # 为什么必须单独存，不能只写进 state
    ///
    /// 曾经踩过：建表失败写进 state 为 Failed(原因)，但下一次探活成功
    /// 又把 state 覆盖成 Ready，于是健康检查报「就绪、已入表 0 条」——
    /// **一个永远召回不到东西的向量层，看起来完全正常**。
    /// 「服务连得上」与「向量表可用」是两件事，必须分开记，
    /// 否则前者会一直把后者的故障冲掉。
    build_error: RwLock<Option<String>>,
}

/// 成功探活的复用窗口（毫秒）。
///
/// 健康检查必须实时（每次真探），否则服务挂了却没人查询时健康检查会一直报 ready。
/// 但**查询路径**不必每次真探一次：一次查询本来就要花一次嵌入往返，
/// 再叠一次探活等于把延迟翻倍。所以这里给「刚刚确认真实可用」一个 1s 的
/// 复用窗口——短到故障能在一个查询内被看见，长到一次查询不会探两遍。
const SUCCESS_CACHE_MS: u64 = 1000;

impl std::fmt::Debug for VectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorIndex")
            .field("endpoint", &self.client.config().endpoint)
            .field("dim", &self.dim())
            .field("vectors", &self.len())
            .field("state", &self.state())
            .field("building", &self.is_building())
            .finish()
    }
}

impl VectorIndex {
    /// 装配。**只探活，不建表**（懒建纪律，见模块文档第 3 条）。
    ///
    /// 探活失败**不 panic、不返回 Err**：状态就是 Failed(原因)，
    /// 查询照常降级。这条与 FullTextIndex::attach 永不返回 Err 是同一条纪律。
    pub fn attach(cfg: VectorConfig) -> Self {
        let client = EmbedderClient::new(cfg);
        let idx = Self {
            client,
            entries: RwLock::new(Vec::new()),
            fingerprint: RwLock::new(String::new()),
            state: RwLock::new(StageState::Absent),
            info: RwLock::new(None),
            retry_after: Mutex::new(None),
            last_ok_at: Mutex::new(None),
            building: std::sync::atomic::AtomicBool::new(false),
            build_error: RwLock::new(None),
        };
        idx.refresh_state();
        idx
    }

    /// 用默认配置装配。
    pub fn attach_default() -> Self {
        Self::attach(VectorConfig::default())
    }

    /// 一个**明确不可用**的实例：不探活、不建表、状态直接是给定的失败原因。
    ///
    /// 供测试与「配置里显式关掉向量层」的场合用。
    pub fn unavailable(reason: impl Into<String>) -> Self {
        let reason: String = reason.into();
        let mut cfg = VectorConfig::default();
        // 只用于拿一个结构完整的空壳，随后立刻把状态钉死，不会用到网络。
        cfg.timeout = std::time::Duration::from_millis(MIN_TIMEOUT_MS);
        let idx = Self {
            client: EmbedderClient::new(cfg),
            entries: RwLock::new(Vec::new()),
            fingerprint: RwLock::new(String::new()),
            state: RwLock::new(StageState::Failed(reason.clone())),
            info: RwLock::new(None),
            retry_after: Mutex::new(Some(
                Instant::now() + std::time::Duration::from_secs(3600),
            )),
            last_ok_at: Mutex::new(None),
            building: std::sync::atomic::AtomicBool::new(false),
            build_error: RwLock::new(Some(reason)),
        };
        idx
    }

    /// 用给定配置装配（供测试注入假服务地址）。
    pub fn with_config(cfg: VectorConfig) -> Self {
        Self::attach(cfg)
    }

    pub fn client(&self) -> &EmbedderClient {
        &self.client
    }

    pub fn state(&self) -> StageState {
        self.state.read().map(|g| g.clone()).unwrap_or(StageState::Absent)
    }

    /// 服务自报的模型身份（没握上手就是 None —— 绝不编一个出来）。
    pub fn info(&self) -> Option<ServiceInfo> {
        self.info.read().ok().and_then(|g| g.clone())
    }

    /// 向量表里的条数。
    pub fn len(&self) -> usize {
        self.entries.read().map(|g| g.len()).unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 向量维度（表为空时是 None）。
    pub fn dim(&self) -> Option<usize> {
        self.entries
            .read()
            .ok()
            .and_then(|g| g.first().map(|e| e.vec.len()))
    }

    /// 表里已建好的资产 id（按建表顺序），供测试与自检。
    pub fn ids(&self) -> Vec<String> {
        self.entries
            .read()
            .map(|g| g.iter().map(|e| e.id.clone()).collect())
            .unwrap_or_default()
    }

    /// 重新探活并刷新状态。**不会建表。**
    ///
    /// 冷却期内直接返回上一次的状态——不重复等一个已知不可用的超时。
    ///
    /// 成功后的 SUCCESS_CACHE_MS 窗口内也直接返回 Ready。**只对成功"缓存"，
    /// 对失败只做冷却**：这两者方向相反，图省事合成一个"上次结果缓存"
    /// 会让故障在窗口期里继续被报成 ready —— 正是本项目最怕的那种谎报。
    fn refresh_state(&self) -> StageState {
        let now = Instant::now();
        if let Ok(g) = self.retry_after.lock() {
            if let Some(t) = *g {
                if now < t {
                    return self.state();
                }
            }
        }
        if self.state().is_usable() && self.build_error().is_none() {
            if let Ok(g) = self.last_ok_at.lock() {
                if let Some(t) = *g {
                    if now.duration_since(t) < std::time::Duration::from_millis(SUCCESS_CACHE_MS) {
                        return StageState::Ready;
                    }
                }
            }
        }
        let state = match self.client.probe() {
            Ok(info) => {
                if let Ok(mut w) = self.info.write() {
                    *w = Some(info);
                }
                if let Ok(mut w) = self.retry_after.lock() {
                    *w = None;
                }
                if let Ok(mut w) = self.last_ok_at.lock() {
                    *w = Some(now);
                }
                // **服务通 ≠ 向量层可用。** 建表曾经失败过、而表仍是空的时，
                // 必须继续报那条建表故障，不能因为「探活通了」就翻成 Ready。
                // 否则会得到一个「报就绪、已入表 0 条、永远召回不到东西」的向量层。
                match self.build_error() {
                    Some(e) if self.is_empty() => StageState::Failed(e),
                    _ => StageState::Ready,
                }
            }
            Err(e) => {
                let until = now + self.client.config().cooldown;
                if let Ok(mut w) = self.retry_after.lock() {
                    *w = Some(until);
                }
                // 失败必须**立刻作废**成功窗口，否则故障会被上一次的成功掩蔽。
                if let Ok(mut w) = self.last_ok_at.lock() {
                    *w = None;
                }
                StageState::Failed(format!(
                    "嵌入服务不可用（{}）：{e}",
                    self.client.config().endpoint
                ))
            }
        };
        if let Ok(mut w) = self.state.write() {
            *w = state.clone();
        }
        state
    }

    /// 实时状态：**绕过成功缓存**，强制真探一次。
    ///
    /// 健康检查必须用这一条。否则服务停了以后，只要没人查询，
    /// 健康检查会一直报 ready —— 而这正是任务里点名要防的「假装 ready」。
    /// 查询路径走 [Self::available_state]，那条允许复用刚确认过的成功。
    pub fn probe_state(&self) -> StageState {
        self.refresh_state_fresh()
    }

    /// 查询路径用的状态判定。与 [Self::probe_state] 只差一处：
    /// 刚确认真实可用时允许复用（见 SUCCESS_CACHE_MS），避免同一次查询探两遍。
    pub fn available_state(&self) -> StageState {
        self.refresh_state()
    }

    /// 无条件真探一次，写回状态。
    fn refresh_state_fresh(&self) -> StageState {
        if let Ok(mut w) = self.last_ok_at.lock() {
            *w = None;
        }
        self.refresh_state()
    }
    /// 确保向量表可用：探活 → 必要时建表/重建。
    ///
    /// 返回表是否可用。**任何失败都只写状态、返回 false**，绝不 panic。
    pub fn ensure<S: AssetStore + ?Sized>(&self, store: &S) -> bool {
        // ① 先看服务在不在。不在就到此为止——连不上时建表毫无意义。
        if !self.refresh_state().is_usable() {
            return false;
        }
        // ② 库侧快照。读不到 = 没有输入，建表没有意义。
        let snapshot = match store.index_source() {
            Ok(s) => s,
            Err(e) => {
                self.set_failed(format!("向量表输入源不可用：{e}"));
                return false;
            }
        };
        let fp = snapshot.fingerprint();
        // ③ 已建好且没有过期 → 直接用。
        {
            let cur = self.fingerprint.read().map(|g| g.clone()).unwrap_or_default();
            if cur == fp && !self.is_empty() {
                return true;
            }
        }
        // ④ 建表。失败只影响本层。
        match self.build(snapshot, fp) {
            Ok(n) => {
                self.set_ready(n);
                n > 0
            }
            Err(e) => {
                self.set_failed(e);
                false
            }
        }
    }

    /// 显式重建（可重建性的入口）。与 ensure 的区别只在于**不看过期判定**。
    pub fn rebuild<S: AssetStore + ?Sized>(&self, store: &S) -> Result<usize, String> {
        if !self.refresh_state().is_usable() {
            return Err(match self.state() {
                StageState::Failed(e) => e,
                other => format!("向量层不可用：{other}"),
            });
        }
        let snapshot = store
            .index_source()
            .map_err(|e| format!("向量表输入源不可用：{e}"))?;
        let fp = snapshot.fingerprint();
        let n = self.build(snapshot, fp)?;
        self.set_ready(n);
        Ok(n)
    }

    fn set_failed(&self, reason: String) {
        // 单独记一份：探活成功会把 state 冲成 Ready，
        // 但**建表失败这个事实不该被冲掉**（见 build_error 字段文档）。
        if let Ok(mut w) = self.build_error.write() {
            *w = Some(reason.clone());
        }
        if let Ok(mut w) = self.state.write() {
            *w = StageState::Failed(reason);
        }
        // 表与指纹一起清掉：留着旧表会让「状态说 failed、查询却用了表」同时成立。
        if let Ok(mut w) = self.entries.write() {
            w.clear();
        }
        if let Ok(mut w) = self.fingerprint.write() {
            w.clear();
        }
    }

    fn set_ready(&self, n: usize) {
        if n > 0 {
            if let Ok(mut w) = self.build_error.write() {
                *w = None;
            }
        }
        let st = if n == 0 {
            // 表建成了但一条可嵌入文本都没有。**这不叫就绪**：
            // 一个空向量表永远召回不到东西，报 ready 就是谎报。
            StageState::Failed("没有可嵌入的资产文本（库为空或文本全空）".to_string())
        } else {
            StageState::Ready
        };
        // 空表同样要记成建表故障：它永远召回不到东西。
        if n == 0 {
            if let Ok(mut w) = self.build_error.write() {
                *w = Some("没有可嵌入的资产文本（库为空或文本全空）".to_string());
            }
        }
        if let Ok(mut w) = self.state.write() {
            *w = st;
        }
    }

    /// 建表层面的失败原因（与「服务连不连得上」无关）。
    pub fn build_error(&self) -> Option<String> {
        self.build_error.read().ok().and_then(|g| g.clone())
    }

    /// 建表本体：嵌入全部资产文本，写进内存表与指纹。
    fn build(&self, snapshot: IndexSnapshot, fp: String) -> Result<usize, String> {
        // 空文本的条目跳过：嵌入空串得到的向量与「什么都不像」没有区别，
        // 放进表里只会给每一类查询都贡献一批恒定的噪声分数。
        let items: Vec<(String, String)> = snapshot
            .entries
            .into_iter()
            .filter(|e| !e.text.trim().is_empty())
            .map(|e| {
                let text = clamp_chars(&e.text, MAX_EMBED_CHARS);
                (e.id, text)
            })
            .collect();

        let mut new_entries: Vec<VectorEntry> = Vec::with_capacity(items.len());
        let mut dim: Option<usize> = None;
        for batch in items.chunks(EMBED_BATCH) {
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            // 建表走**建表预算**：冷启动首次前向实测 ~7s，
            // 拿查询的 2.5s 去卡它会让每次冷启动的首次建表必然失败。
            let vectors = self.client.embed_for_build(&texts)?;
            if vectors.len() != batch.len() {
                return Err(format!(
                    "嵌入条数不符：本批 {} 条，返回 {} 条",
                    batch.len(),
                    vectors.len()
                ));
            }
            for ((id, text), vec) in batch.iter().zip(vectors.into_iter()) {
                match dim {
                    None => dim = Some(vec.len()),
                    Some(d) if d != vec.len() => {
                        return Err(format!("同一批向量维度不一致：{d} vs {}", vec.len()))
                    }
                    Some(_) => {}
                }
                new_entries.push(VectorEntry {
                    id: id.clone(),
                    text: text.clone(),
                    vec,
                });
            }
        }

        // 全部批次都成功才换表 —— 中途失败不会留下半张表。
        let n = new_entries.len();
        if let Ok(mut w) = self.entries.write() {
            *w = new_entries;
        }
        if let Ok(mut w) = self.fingerprint.write() {
            *w = fp;
        }
        Ok(n)
    }

    /// 召回：给查询文本，返回 (资产 id, 余弦分)，按分数降序。
    ///
    /// 表为空、维度不符、余弦算不出来 —— 一律返回 Err 或空，
    /// **绝不返回一个「看起来正常但其实是垃圾」的排序**。
    pub fn recall(&self, query: &str, top_k: usize) -> Result<Vec<Scored>, String> {
        let q = query.trim();
        if q.is_empty() || top_k == 0 {
            return Ok(Vec::new());
        }
        let qvec = self
            .client
            .embed(&[clamp_chars(q, MAX_EMBED_CHARS)])
            .map_err(|e| format!("查询嵌入失败：{e}"))?
            .into_iter()
            .next()
            .ok_or_else(|| "查询嵌入返回空".to_string())?;

        let guard = self
            .entries
            .read()
            .map_err(|_| "向量表锁已中毒".to_string())?;
        if guard.is_empty() {
            return Err("向量表为空（未建表或建表失败）".to_string());
        }
        let mut scored: Vec<Scored> = Vec::with_capacity(guard.len());
        for e in guard.iter() {
            match cosine(&qvec, &e.vec) {
                Some(s) => scored.push(Scored {
                    id: e.id.clone(),
                    score: s,
                }),
                // 维度和建表时不一致（服务换了模型）→ 直接失败，不静默跳过。
                None => {
                    return Err(format!(
                        "查询向量维度 {} 与资产 {} 的 {} 不一致（嵌入模型可能已更换）",
                        qvec.len(),
                        e.id,
                        e.vec.len()
                    ))
                }
            }
        }
        // 排序键 (分数降序, id 升序)：id 是确定性兜底，杜绝「同分两次顺序不同」。
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        scored.truncate(top_k);
        Ok(scored)
    }

    /// 表里某条资产在建表时用的文本（供测试证明「嵌入的是哪段文本」）。
    pub fn text_of(&self, id: &str) -> Option<String> {
        self.entries
            .read()
            .ok()
            .and_then(|g| g.iter().find(|e| e.id == id).map(|e| e.text.clone()))
    }
}

// ─────────────────────────────────────────────────────────────────
// 查询路径的「不阻塞」纪律
// ─────────────────────────────────────────────────────────────────
//
// 上面这套 ensure() 是**阻塞**的：它探活 → 取快照 → 嵌入全表。
// 在嵌入服务健康时这没问题（实测 506 条约 12s，只在冷启动发生一次）。
// 但有一个反例会把整条查询拖死：**服务探活正常、/embed 却卡住**
// （GPU 被别的进程占满时就是这一种，实测 /health 90ms 应答、
// /embed 45s 不回）。这时 ensure() 会一直等到建表预算耗尽，
// 而调用它的是一次**用户查询**。
//
// 这与项目最硬的那条不变量直接冲突：
//   「向量层缺席/超时/报错时，查询必须退回结构化基线 + BM25」。
// 退回得**及时**才算退回；卡 60 秒再退回，用户看到的就是「服务卡死了」。
//
// 所以查询路径走 [VectorIndex::ensure_nonblocking]：它最多只等
// 一个查询预算（cfg.timeout），等不到就把建表**留在后台继续**，
// 本次查询立刻降级并如实说明「建表中」。下一次查询若表已就绪就直接用。

impl VectorIndex {
    /// 查询路径专用：**绝不阻塞超过一个查询预算**。
    ///
    /// 三种结局：
    /// - 已有就绪的表 → true，本次查询用向量；
    /// - 没表 / 表过期 → 启动（或复用）后台建表，**本次查询降级**；
    /// - 服务不可达 → false，降级。
    ///
    /// 注意返回值与「本次是否用上了向量」是同一件事：
    /// true 才代表这次查询真的走了向量召回。
    pub fn ensure_nonblocking<S: AssetStore + ?Sized>(self: &Arc<Self>, store: &S) -> bool {
        // ① 服务在不在。这一步本身是廉价的（/health，实测 14-90ms）。
        //
        //    这里问的是**服务可达性**，不是「向量层整体可用」。两者必须分开：
        //    建表失败会让整体不可用，但服务仍然是通的，于是**下一次查询
        //    应该再试一次建表**——否则一次失败就把向量层永久锁死，
        //    只能重启进程才能恢复（没有任何人期待这种语义）。
        //    注意 probe_state() 会把「建表失败」也报成 failed，那会让这里
        //    在第一次建表失败后**永久放弃重试**。所以先看服务身份还在不在：
        //    能拿到 info 就说明服务通着，值得再试一次建表。
        if self.probe_state().is_failed() && self.info().is_none() {
            return false;
        }
        // ② 库侧快照。**同步读**：它只是把资产文本取出来，不碰嵌入，
        //    实测 506 条约 10ms。真正贵的是嵌入，那一步才要挪到后台。
        let snapshot = match store.index_source() {
            Ok(s) => s,
            Err(e) => {
                self.set_failed(format!("向量表输入源不可用：{e}"));
                return false;
            }
        };
        let fp = snapshot.fingerprint();

        // ③ 表已就绪且没过期 → 直接用。
        {
            let cur = self.fingerprint.read().map(|g| g.clone()).unwrap_or_default();
            if cur == fp && !self.is_empty() {
                return true;
            }
        }

        // ④ 没表 / 过期 → 后台建表。**不在这里等它建完。**
        self.spawn_build(snapshot, fp);
        false
    }

    /// 起一个后台建表线程；已有线程在跑就不重复起。
    fn spawn_build(self: &Arc<Self>, snapshot: IndexSnapshot, fp: String) {
        // 同一时刻只允许一个建表线程：两个线程同时嵌入会把 GPU 显存
        // 与请求配额翻倍，而且后完成的那个会覆盖先完成的，纯浪费。
        if self
            .building
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("ac-vector-build".into())
            .spawn(move || {
                let result = this.build(snapshot, fp);
                match result {
                    Ok(n) => this.set_ready(n),
                    Err(e) => this.set_failed(e),
                }
                this.building.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            // 线程都起不来（极端资源耗尽）：如实降级，绝不假装在建。
            self.building.store(false, Ordering::SeqCst);
            self.set_failed("无法启动后台建表线程（系统资源耗尽）".to_string());
        }
    }

    /// 是否正在后台建表。健康检查用它给出「建表中」这个**确切**状态，
    /// 而不是含糊的 failed 或过早的 ready。
    pub fn is_building(&self) -> bool {
        self.building.load(Ordering::SeqCst)
    }

    /// 强制设定状态（**仅供测试**构造特定的降级形态）。
    pub fn set_state_for_test(&self, st: StageState) {
        if let Ok(mut w) = self.state.write() {
            *w = st;
        }
    }
}

/// 一条向量召回结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub id: String,
    pub score: f32,
}

/// 按字符（不是字节）截断，避免把中文切在 UTF-8 边界外。
fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

// ─────────────────────────────────────────────────────────────────
// 测试支撑：一个真的监听回环端口的假嵌入服务
// ─────────────────────────────────────────────────────────────────
//
// 为什么不用 mock 替身：本模块最容易出错的地方恰恰是 **HTTP 那一层**
// （分块编码、截断、状态码、超时）。把它 mock 掉，测的就只剩我自己写的那个
// mock。所以这里起一个真的 TcpListener，走真的 socket、真的 HTTP/1.1。
//
// 为什么向量是「按字符分桶」而不是随机数：随机向量之间余弦≈0，
// 什么排序都能过测试。分桶向量让**文本重叠度真的映射成余弦**，
// 于是「召回到了该召回的」变成一条有鉴别力的断言。
//
// **为什么不是 cfg(test)**：下游 crate（ac-mcp）的健康检查测试
// 需要同一个假嵌入服务。若只在 test 配置下编译，跨 crate 就够不到它。
// 特征开关让它成为**一份共享的测试设施**，默认关闭。
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::*;
    use std::io::{BufRead, BufReader, Read as IoRead};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 假服务的响应模式。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Mode {
        Ok,
        Loading,
        WrongCount,
        RaggedDim,
        InfiniteVector,
        NullElement,
        ServerError,
        Hang,
    }

    /// 分桶向量：按字符码位分桶计数，再 L2 归一。
    pub fn bucket_embed(text: &str, dim: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        for c in text.chars() {
            let i = (c as u32 as usize) % dim;
            v[i] += 1.0;
        }
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            for x in v.iter_mut() {
                *x /= n;
            }
        }
        v
    }

    pub const TEST_DIM: usize = 64;

    pub struct FakeEmbedder {
        pub endpoint: String,
        stop: Arc<AtomicBool>,
        pub requests: Arc<AtomicUsize>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeEmbedder {
        pub fn start(mode: Mode) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("绑定回环端口");
            let port = listener.local_addr().expect("本地地址").port();
            listener.set_nonblocking(true).expect("设为非阻塞");
            let stop = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(AtomicUsize::new(0));
            let s2 = stop.clone();
            let r2 = requests.clone();
            let handle = std::thread::spawn(move || {
                while !s2.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            r2.fetch_add(1, Ordering::Relaxed);
                            std::thread::spawn(move || {
                                let _ = serve_one(stream, mode);
                            });
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                endpoint: format!("http://127.0.0.1:{port}"),
                stop,
                requests,
                handle: Some(handle),
            }
        }

        pub fn config(&self, timeout_ms: u64) -> VectorConfig {
            VectorConfig {
                endpoint: self.endpoint.clone(),
                timeout: Duration::from_millis(timeout_ms),
                build_timeout: Duration::from_millis(10_000),
                cooldown: Duration::from_millis(0),
            }
        }

        pub fn index(&self, timeout_ms: u64) -> VectorIndex {
            VectorIndex::attach(self.config(timeout_ms))
        }
    }

    impl Drop for FakeEmbedder {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Ok(addr) = self.endpoint.trim_start_matches("http://").parse() {
                let _ = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(50));
            }
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn serve_one(mut stream: std::net::TcpStream, mode: Mode) -> std::io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let t = line.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some((k, v)) = t.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body)?;
        }
        let path = request_line.split_whitespace().nth(1).unwrap_or("/");

        if mode == Mode::Hang {
            std::thread::sleep(Duration::from_secs(3));
            return Ok(());
        }

        let response = if path.starts_with("/health") {
            let payload = match mode {
                Mode::Loading => {
                    format!(r#"{{"status":"loading","model":"fake","dim":{TEST_DIM}}}"#)
                }
                Mode::ServerError => {
                    return write_raw(&mut stream, 500, r#"{"error":"boom"}"#);
                }
                Mode::Hang => unreachable!(),
                _ => format!(r#"{{"status":"ok","model":"fake-embedder","dim":{TEST_DIM}}}"#),
            };
            write_ok(&mut stream, &payload)
        } else if path.starts_with("/embed") {
            let req: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            let texts: Vec<String> = req
                .get("texts")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .map(|x| x.as_str().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            match mode {
                Mode::ServerError => {
                    return write_raw(&mut stream, 500, r#"{"error":"boom"}"#);
                }
                Mode::WrongCount => {
                    let vecs: Vec<serde_json::Value> = texts
                        .iter()
                        .take(texts.len().saturating_sub(1))
                        .map(|t| json_vec(&bucket_embed(t, TEST_DIM)))
                        .collect();
                    let payload = serde_json::json!({
                        "vectors": vecs, "dim": TEST_DIM, "count": vecs.len()
                    })
                    .to_string();
                    return write_ok(&mut stream, &payload);
                }
                Mode::RaggedDim => {
                    let vecs: Vec<serde_json::Value> = texts
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            let d = if i == 0 { TEST_DIM } else { TEST_DIM / 2 };
                            json_vec(&bucket_embed(t, d))
                        })
                        .collect();
                    let payload = serde_json::json!({
                        "vectors": vecs, "dim": TEST_DIM, "count": vecs.len()
                    })
                    .to_string();
                    return write_ok(&mut stream, &payload);
                }
                Mode::InfiniteVector => {
                    let mut v =
                        bucket_embed(texts.first().map(String::as_str).unwrap_or(""), TEST_DIM);
                    v[0] = 1.0;
                    let mut arr: Vec<String> = v.iter().map(|x| format!("{x}")).collect();
                    arr[0] = "1e300".to_string();
                    let payload = format!(
                        r#"{{"vectors":[[{}]],"dim":{TEST_DIM},"count":1}}"#,
                        arr.join(",")
                    );
                    return write_ok(&mut stream, &payload);
                }
                Mode::NullElement => {
                    let mut arr: Vec<String> =
                        bucket_embed(texts.first().map(String::as_str).unwrap_or(""), TEST_DIM)
                            .iter()
                            .map(|x| format!("{x}"))
                            .collect();
                    arr[0] = "null".to_string();
                    let payload = format!(
                        r#"{{"vectors":[[{}]],"dim":{TEST_DIM},"count":1}}"#,
                        arr.join(",")
                    );
                    return write_ok(&mut stream, &payload);
                }
                _ => {
                    let vecs: Vec<serde_json::Value> = texts
                        .iter()
                        .map(|t| json_vec(&bucket_embed(t, TEST_DIM)))
                        .collect();
                    let payload = serde_json::json!({
                        "vectors": vecs, "dim": TEST_DIM, "count": vecs.len()
                    })
                    .to_string();
                    write_ok(&mut stream, &payload)
                }
            }
        } else {
            write_raw(&mut stream, 404, r#"{"error":"not found"}"#)
        };
        response
    }

    fn json_vec(v: &[f32]) -> serde_json::Value {
        serde_json::Value::Array(
            v.iter()
                .map(|x| match serde_json::Number::from_f64(*x as f64) {
                    Some(n) => serde_json::Value::Number(n),
                    None => serde_json::Value::Null,
                })
                .collect(),
        )
    }

    fn write_ok(stream: &mut std::net::TcpStream, body: &str) -> std::io::Result<()> {
        write_raw(stream, 200, body)
    }

    fn write_raw(stream: &mut std::net::TcpStream, status: u16, body: &str) -> std::io::Result<()> {
        let resp = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        use std::io::Write as IoWrite;
        stream.write_all(resp.as_bytes())?;
        stream.flush()
    }

    pub fn dead_endpoint() -> String {
        let l = TcpListener::bind("127.0.0.1:0").expect("绑定");
        let port = l.local_addr().expect("地址").port();
        drop(l);
        format!("http://127.0.0.1:{port}")
    }

    #[allow(dead_code)]
    fn _assert_read_trait<R: IoRead>(_: R) {}
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::memstore::InMemoryStore;
    use crate::store::AssetRow;
    use ac_core::Authority;

    fn asset(id: &str, name: &str, text: &str) -> AssetRow {
        let mut r = AssetRow::new(id, "skill", name, Authority::Read);
        r.text = format!("{name} {text}");
        r
    }

    fn demo_store() -> InMemoryStore {
        InMemoryStore::new(vec![
            asset("s::seq", "画时序图", "把消息流转画成时序图，参与者与消息顺序"),
            asset("s::flow", "画流程图", "流程图与分支判断，节点连线"),
            asset("s::db", "数据库备份", "备份与恢复脚本，定时任务与校验"),
            asset("s::net", "网络诊断", "排查端口连通性与路由表"),
        ])
    }

    #[test]
    fn cosine_is_one_for_identical_and_zero_for_disjoint() {
        let a = bucket_embed("画时序图", TEST_DIM);
        assert!((cosine(&a, &a).unwrap() - 1.0).abs() < 1e-5);
        let b = bucket_embed("zzz", TEST_DIM);
        assert!(cosine(&a, &b).unwrap().abs() < 1e-5, "无共享字符必须正交");
    }

    #[test]
    fn cosine_returns_none_instead_of_zero_when_it_cannot_compute() {
        assert!(cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]).is_none());
        assert!(cosine(&[], &[]).is_none());
        assert!(cosine(&[0.0, 0.0], &[1.0, 0.0]).is_none());
    }

    #[test]
    fn default_config_points_at_the_embedder_and_has_a_short_timeout() {
        let c = VectorConfig::default();
        assert_eq!(c.endpoint, "http://127.0.0.1:18191");
        assert!(
            c.timeout >= Duration::from_secs(2) && c.timeout <= Duration::from_secs(3),
            "查询超时必须落在 2-3s，实际 {:?}",
            c.timeout
        );
        assert!(
            c.build_timeout > c.timeout,
            "建表预算必须大于查询预算：{:?} vs {:?}",
            c.build_timeout,
            c.timeout
        );
    }

    #[test]
    fn bad_timeout_env_falls_back_to_default_instead_of_becoming_zero() {
        assert!(!(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&0));
        assert!(!(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&999_999));
        assert!((MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&DEFAULT_TIMEOUT_MS));
    }

    #[test]
    fn url_splitting_accepts_only_http_and_rejects_garbage() {
        let (h, p, path) = rawhttp::split_for_test("http://127.0.0.1:18191/health").unwrap();
        assert_eq!((h.as_str(), p, path.as_str()), ("127.0.0.1", 18191, "/health"));
        let (h, p, path) = rawhttp::split_for_test("http://example/").unwrap();
        assert_eq!((h.as_str(), p, path.as_str()), ("example", 80, "/"));
        assert!(rawhttp::split_for_test("https://127.0.0.1:18191/health").is_err());
        assert!(rawhttp::split_for_test("127.0.0.1:18191").is_err());
        assert!(rawhttp::split_for_test("http://u:p@127.0.0.1:18191/").is_err());
        assert!(rawhttp::split_for_test("http://127.0.0.1:abc/").is_err());
    }

    #[test]
    fn response_parser_handles_content_length_chunked_and_truncation() {
        let r =
            rawhttp::parse_for_test(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhiEXTRA").unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, "hi");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n3\r\nabc\r\n0\r\n\r\n";
        assert_eq!(rawhttp::parse_for_test(chunked).unwrap().body, "helloabc");
        assert!(
            rawhttp::parse_for_test(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc").is_err()
        );
        assert!(rawhttp::parse_for_test(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n").is_err());
    }

    #[test]
    fn probe_is_ready_only_when_service_says_ok() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = svc.index(2000);
        let st = idx.probe_state();
        assert!(st.is_usable(), "服务报 ok 时必须 ready，实际 {st}");
        let info = idx.info().expect("就绪时必须拿到服务身份");
        assert_eq!(info.model, "fake-embedder");
        assert_eq!(info.dim, Some(TEST_DIM));
    }

    #[test]
    fn probe_reports_failed_when_model_still_loading() {
        let svc = FakeEmbedder::start(Mode::Loading);
        let idx = svc.index(2000);
        let st = idx.probe_state();
        assert!(st.is_failed(), "加载中必须报 failed，实际 {st}");
        assert!(
            st.to_string().contains("loading"),
            "原因必须带服务自报状态，实际 {st}"
        );
        assert!(idx.info().is_none(), "没就绪就不许有身份");
    }

    #[test]
    fn probe_reports_failed_with_reason_when_nothing_is_listening() {
        let mut cfg = VectorConfig::default();
        cfg.endpoint = dead_endpoint();
        cfg.timeout = Duration::from_millis(300);
        cfg.cooldown = Duration::from_millis(0);
        let idx = VectorIndex::attach(cfg);
        let st = idx.probe_state();
        assert!(st.is_failed(), "连不上必须报 failed，实际 {st}");
        assert!(
            st.to_string().contains("127.0.0.1"),
            "失败原因必须带地址，实际 {st}"
        );
    }

    #[test]
    fn probe_times_out_instead_of_hanging_when_service_never_replies() {
        let svc = FakeEmbedder::start(Mode::Hang);
        let idx = svc.index(300);
        let started = Instant::now();
        let st = idx.probe_state();
        let elapsed = started.elapsed();
        assert!(st.is_failed(), "不回包必须报 failed，实际 {st}");
        assert!(
            elapsed < Duration::from_secs(2),
            "超时预算 300ms，实际花了 {elapsed:?}"
        );
    }

    #[test]
    fn attach_does_not_build_the_index_but_probe_does_happen() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = svc.index(2000);
        assert!(idx.is_empty(), "装配时不得建表");
        assert!(idx.probe_state().is_usable(), "装配时必须已探活");
    }

    #[test]
    fn ensure_builds_lazily_and_is_idempotent() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let store = demo_store();
        let idx = svc.index(2000);
        assert!(idx.ensure(&store));
        assert_eq!(idx.len(), 4);
        let before = idx.ids();
        assert!(idx.ensure(&store));
        assert_eq!(idx.ids(), before, "内容没变就不该重建");
    }

    #[test]
    fn ensure_rebuilds_when_the_store_content_changes() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = svc.index(2000);
        assert!(idx.ensure(&demo_store()));
        assert_eq!(idx.len(), 4);
        let mut rows = vec![
            asset("s::seq", "画时序图", "把消息流转画成时序图"),
            asset("s::flow", "画流程图", "流程图与分支判断"),
            asset("s::db", "数据库备份", "备份与恢复脚本"),
            asset("s::net", "网络诊断", "排查端口连通性"),
        ];
        rows.push(asset("s::new", "新增资产", "新加的一条组件"));
        assert!(idx.ensure(&InMemoryStore::new(rows)));
        assert_eq!(idx.len(), 5);
        assert!(idx.ids().contains(&"s::new".to_string()));
    }

    #[test]
    fn explicit_rebuild_works_without_waiting_for_expiry() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let store = demo_store();
        let idx = svc.index(2000);
        assert_eq!(idx.rebuild(&store).unwrap(), 4);
    }

    #[test]
    fn build_failure_leaves_the_service_usable_not_panicking() {
        let svc = FakeEmbedder::start(Mode::ServerError);
        let store = demo_store();
        let idx = svc.index(2000);
        assert!(!idx.ensure(&store));
        assert!(idx.state().is_failed());
        assert!(idx.is_empty(), "失败的建表不得留下半张表");
    }

    #[test]
    fn wrong_vector_count_is_rejected_not_misaligned() {
        let svc = FakeEmbedder::start(Mode::WrongCount);
        let idx = svc.index(2000);
        assert!(!idx.ensure(&demo_store()));
        assert!(
            idx.state().to_string().contains("条数不符"),
            "实际 {}",
            idx.state()
        );
    }

    #[test]
    fn ragged_dimensions_are_rejected() {
        let svc = FakeEmbedder::start(Mode::RaggedDim);
        let idx = svc.index(2000);
        assert!(!idx.ensure(&demo_store()));
        assert!(idx.state().to_string().contains("维度"), "实际 {}", idx.state());
    }

    #[test]
    fn non_finite_vector_elements_are_rejected() {
        let store = InMemoryStore::new(vec![asset("s::a", "画时序图", "把消息流转画成时序图")]);
        let svc = FakeEmbedder::start(Mode::InfiniteVector);
        let idx = svc.index(2000);
        assert!(!idx.ensure(&store));
        assert!(
            idx.state().to_string().contains("NaN/Inf"),
            "实际 {}",
            idx.state()
        );
        let svc2 = FakeEmbedder::start(Mode::NullElement);
        let idx2 = svc2.index(2000);
        assert!(!idx2.ensure(&store));
        assert!(
            idx2.state().to_string().contains("非数值"),
            "实际 {}",
            idx2.state()
        );
    }

    #[test]
    fn recall_ranks_the_textually_closest_asset_first() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let store = demo_store();
        let idx = svc.index(2000);
        assert!(idx.ensure(&store));
        let hits = idx.recall("画时序图", 3).expect("召回");
        assert_eq!(
            hits[0].id,
            "s::seq",
            "实际 {:?}",
            hits.iter().map(|h| (&h.id, h.score)).collect::<Vec<_>>()
        );
        assert!(hits[0].score > hits[1].score, "前两名必须有分差");
    }

    #[test]
    fn recall_on_an_empty_index_errors_instead_of_claiming_no_matches() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = svc.index(2000);
        let e = idx.recall("画时序图", 3).unwrap_err();
        assert!(e.contains("为空"), "实际 {e}");
    }

    #[test]
    fn recall_respects_top_k() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let store = demo_store();
        let idx = svc.index(2000);
        assert!(idx.ensure(&store));
        assert_eq!(idx.recall("画图", 2).unwrap().len(), 2);
        assert_eq!(idx.recall("画图", 0).unwrap().len(), 0);
        assert!(idx.recall("   ", 5).unwrap().is_empty());
    }

    #[test]
    fn build_uses_the_assets_searchable_text_not_the_id() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let store = demo_store();
        let idx = svc.index(2000);
        assert!(idx.ensure(&store));
        let t = idx.text_of("s::seq").expect("表里必须有 s::seq");
        assert!(t.contains("时序图"), "嵌入的必须是可检索文本，实际 {t}");
        assert_ne!(t, "s::seq");
    }

    #[test]
    fn empty_text_assets_are_skipped_instead_of_polluting_the_table() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let mut blank = AssetRow::new("s::blank", "skill", "空文本", Authority::Read);
        blank.text = "   ".to_string();
        let store = InMemoryStore::new(vec![
            asset("s::seq", "画时序图", "把消息流转画成时序图"),
            blank,
        ]);
        let idx = svc.index(2000);
        assert!(idx.ensure(&store));
        assert_eq!(idx.len(), 1, "空文本资产不得进表");
    }

    #[test]
    fn an_all_blank_store_reports_failed_not_ready() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let mut blank = AssetRow::new("s::blank", "skill", "空", Authority::Read);
        blank.text = "  ".to_string();
        let idx = svc.index(2000);
        assert!(!idx.ensure(&InMemoryStore::new(vec![blank])), "空表不算可用");
        assert!(
            idx.state().is_failed(),
            "空表必须报 failed，实际 {}",
            idx.state()
        );
    }

    #[test]
    fn unavailable_constructor_never_touches_the_network() {
        let idx = VectorIndex::unavailable("配置里关掉了");
        assert!(idx.state().is_failed());
        assert!(idx.state().to_string().contains("配置里关掉了"));
        assert!(idx.probe_state().is_failed());
    }

    #[test]
    fn nonblocking_ensure_returns_immediately_and_builds_in_background() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let store = demo_store();
        let idx = std::sync::Arc::new(svc.index(2000));
        assert!(idx.is_empty());
        let started = Instant::now();
        let used = idx.ensure_nonblocking(&store);
        let elapsed = started.elapsed();
        assert!(!used, "首次调用不该报告「已用上向量」");
        assert!(
            elapsed < Duration::from_millis(500),
            "非阻塞路径花了 {elapsed:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && idx.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(idx.len(), 4, "后台建表必须真的完成");
        assert!(idx.ensure_nonblocking(&store), "表就绪后必须直接可用");
    }

    #[test]
    fn nonblocking_ensure_does_not_spawn_duplicate_builds() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let store = demo_store();
        let idx = std::sync::Arc::new(svc.index(2000));
        for _ in 0..5 {
            idx.ensure_nonblocking(&store);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && idx.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(idx.len(), 4);
        assert!(!idx.is_building(), "建完之后 building 标志必须复位");
    }

    /// **回归测试**：曾经的真实 bug。
    ///
    /// 建表失败写进 state=Failed(原因)，但下一次探活成功又把 state
    /// 覆盖成 Ready —— 于是健康检查报「就绪、已入表 0 条」，
    /// 一个永远召回不到东西的向量层看起来完全正常。
    #[test]
    fn build_failure_is_not_masked_by_a_later_successful_probe() {
        let svc = FakeEmbedder::start(Mode::Ok);
        let idx = std::sync::Arc::new(svc.index(2000));

        idx.set_failed("建表失败（模拟）".to_string());
        assert!(idx.is_empty());
        assert!(
            idx.build_error().is_some(),
            "建表失败必须被单独记下来，否则会被探活冲掉"
        );

        let st = idx.probe_state();
        assert!(st.is_failed(), "探活成功不能把建表失败洗白，实际 {st}");
        assert!(
            st.to_string().contains("建表失败"),
            "报出来的必须是建表那条原因，实际 {st}"
        );

        let store = demo_store();
        assert!(!idx.ensure_nonblocking(&store));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && idx.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(idx.len(), 4);
        assert!(idx.build_error().is_none(), "表建好后失败原因必须清掉");
        assert!(idx.probe_state().is_usable(), "表建好后应当真的 ready");
    }
}
