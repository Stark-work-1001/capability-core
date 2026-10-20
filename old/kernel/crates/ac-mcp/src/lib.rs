//! # ac-mcp
//!
//! **对外服务接口 + 多智能体路由协调**。本 crate 有两个消费者：
//!
//! 1. 外部对话智能体（"帮我找个能画图的工具"）——它需要一个稳定接口来
//!    **在中枢里找工具**，而不是把整个库搬到自己进程里。
//! 2. 多个同时接入的智能体——它们各自只应看到被授权的能力子集，
//!    抢同一份可变资源时要有确定的行为。
//!
//! ## 两个对外形态，一套内核
//!
//! - [`mcp`]：MCP（Model Context Protocol）服务端，stdio transport。
//!   这是与主流智能体软件对接的**标准方式**，所以做成 stdio——
//!   宿主按子进程拉起它，不需要常驻端口，也就不可能与别人的服务撞端口。
//! - [`http`]：HTTP/1.1 + JSON，给不方便走 MCP 的调用方（脚本、
//!   其它语言、监控）。
//!
//! 两者**共用** [`gateway`]。这是刻意的：两条链路一旦各写一份查询逻辑，
//! 它们就会在某次改动后给出不同答案，而"同一个问题两个接口答案不一样"
//! 是最难排查的一类故障。
//!
//! ## 只读承诺
//!
//! [`gateway::CapabilityGateway`] **没有任何**会改动 assets /
//! capabilities 表的方法：它只有 search / detail / tags / health 四类
//! 读操作，由 guard_gateway_exposes_no_asset_mutation 守门。
//!
//! 唯一会写库的是**协调元数据**（agents / agent_capability_usage），
//! 那不是"用户资产"，而是接入关系本身——不记它，多智能体协调就无从审计。

pub mod caller;
pub mod faults_api;
pub mod gateway;
pub mod http;
pub mod mcp;
pub mod router;
pub mod service;
pub mod scheduler;
pub mod workflows;
pub mod search_modes;

pub use caller::{Caller, CallerIdentity, Grant, Visibility, VisibilityStats};
pub use gateway::{
    CapabilityDetail, CapabilityGateway, CapabilityRow, ComponentHealth, GatewayError, HealthReport,
    SearchParams, SearchResult, StageHealth, VersionInfo,
};
pub use http::{HttpRequest, HttpResponse, HttpServer, DEFAULT_PORT};
pub use mcp::{McpServer, RpcError, MCP_PROTOCOL_VERSION};
pub use service::{Audited, TrimmedSearchResult};
pub use router::{Allocation, Lease, LeaseMode, Router, RouterError, MAX_QUEUE};
pub use faults_api::{RepairTask, RepairTaskList, SweepSummary};
pub use service::CapabilityService;

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

/// 对外接口版本号。破坏性变更时递增（HTTP 路径与 MCP serverInfo 都用它）。
pub const API_VERSION: &str = "v1";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_loads() {
        assert!(!super::CRATE_NAME.is_empty());
    }

    /// 只读守门：本 crate 的网关实现里不得出现对资产表的写操作。
    ///
    /// 为什么用源码扫描而不是只靠"我没有写这些方法"的自述：
    /// 一次重构很容易顺手加一句 UPDATE assets，而 API 面上看不出来。
    /// 代价是要维护关键词表，所以只盯最要命的表名和动词。
    #[test]
    fn guard_gateway_exposes_no_asset_mutation() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gateway.rs"))
            .expect("读 gateway.rs");
        let code: String = src
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.starts_with("//") {
                    String::new()
                } else {
                    t.split("//").next().unwrap_or("").to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("
");
        for forbidden in ["INSERT INTO assets", "UPDATE assets", "DELETE FROM assets"] {
            assert!(
                !code.contains(forbidden),
                "对外接口必须只读：gateway.rs 出现了 {forbidden}"
            );
        }
    }
}
