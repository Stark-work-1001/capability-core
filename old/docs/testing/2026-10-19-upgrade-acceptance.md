# Capability Core 开发升级验收记录

交付范围为开发目录和隔离候选包；日常使用目录 `C:/work/capability-core-standalone` 未执行更新。

## 验证

- Rust 工作区：531 项测试通过，覆盖进程误判、心跳、模型配置并发、MCP 会话隔离、导入、事务、版本回滚、来源边界和枚举序列化契约。
- React / TypeScript：150 项测试通过，TypeScript 检查和 Vite 构建通过。保留既有 React act 提示。
- 隔离链路：46 项检查通过。`scripts/verify-upgrade.py` 使用独立数据库、源目录和端口，验证五路检索、十六项工作流、模型失败降级、生成与验收、语义比较与合并草稿、HTTP MCP、并发请求、服务运行期间新增能力的索引更新、审计和完整性拒绝。
- 目录迁移：验证纳管入口使用相对路径，vendor 内容未变化时重新扫描仍刷新来源路径。
- 桌面：实际启动、候选版标识、18297 端口、首页、工作流、模型卡片与维修站通过界面检查。自检使用候选包独立目录。
- WebView2：官方固定运行时逐文件校验后离线部署到本地缓存；系统部署助手使用 Windows 自带 PowerShell，不改变系统运行时、不关闭浏览器沙箱。

模型链路使用本机模拟 OpenAI 兼容接口；未配置真实密钥，不能据此声明真实云端服务、模型效果和真实资产修复已经验收。当前资产的外部 CAD、Python 包、MCP 远程服务等依赖仍需逐项预检，随包运行时仅覆盖中枢本身。

## 复现

在项目根目录运行：

```powershell
cargo test --manifest-path kernel/Cargo.toml --workspace --quiet
npm --prefix apps/desktop test -- --reporter=dot
python scripts/verify-upgrade.py --exe kernel/target/release/assets-serve.exe --out artifacts/acceptance-new-run
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-candidate.ps1
```

`--out` 必须使用新的目录，脚本不会覆盖既有验收数据。候选包路径记录在 `artifacts/latest-candidate.txt`。

## 审核与升级

用户审核候选版后，才使用 `scripts/promote-candidate.ps1 -ReviewedStandalone -CandidateRoot <已审核候选目录>`。此脚本先验证程序文件摘要，保留旧程序文件作为回退副本；不复制开发数据库或候选数据库，不覆盖既有模型凭据，不复制候选标记。日常版使用自身数据库和 18198 端口。

来源与协议范围见 [开发升级记录](../planning/2026-10-18-upgrade.md)。部分协议仅有声明吸收，未冒充完整协议执行。
