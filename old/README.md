# 旧开发目录（已冻结，仅供参照）

> **状态：已冻结，不再维护。**
> 保留它是因为里面有真实的踩坑记录和设计演变过程，对理解现在的 Capability Core
> 为什么长成这样有帮助。**不要基于它开发新功能。**

原始 README 保留在 `README.original.md`（同样经过脱敏）。

---

## 这是什么

Capability Core 的前一代实现，代号 *Capability Nexus*，版本 v0.1.0。
技术栈与现在完全不同：

| | 旧开发目录（本目录） | 现行版本 |
| --- | --- | --- |
| 桌面壳 | Tauri 2 | Electron 44 |
| 内核 | Rust workspace（6 个 crate） | Node.js 24 |
| 界面 | React + TypeScript | React 19 + TypeScript |
| 检索 | Tantivy 全文索引 + 向量 | SQLite FTS5 + trigram + 云端向量 |
| 数据库 | SQLite（registry.db） | SQLite（node:sqlite，同名不同结构） |
| 模型接入 | 槽位制（slot-based） | 角色路由（role-based） |

**为什么放弃**：Rust 内核 + Tauri 要在每台机器上单独准备工具链，
而当时真正的瓶颈是「能力归一模型」与「维修闭环」的设计，不是运行时性能。
换到 Electron + Node 后可直接复用现有 Node 生态，迭代快了一个量级。

---

## 目录内容（190 个文件，约 2.7 MB）

只保留**源码、协议定义与文档**：

```
old/
  apps/desktop/     Tauri 桌面应用（React 前端 + Rust src-tauri）
  kernel/           Rust workspace，6 个 crate
    ac-core           协议层，零 IO、零依赖
    ac-registry       资产注册
    ac-retrieval      检索（经 trait 抽象存储）
    ac-mcp            MCP 服务端
    ac-llm            模型槽位
    ac-ipc            类型转换
  proto/            Capability Manifest v1 协议定义
  assets/           旧资产目录（示例 MCP + 技能）
  embedder/         本地嵌入服务（Python）
  scripts/          构建与发布脚本
  docs/             五类文档（调研 / 规划 / 改动 / 测试 / 经验）
  data/             仅 llm-providers.json（槽位配置样例）
```

### 为控制体积而剥离的内容

| 剥离项 | 原体积 | 原因 |
| --- | --- | --- |
| `artifacts/` | 8.4 GB | 验收产物，含多份完整运行时副本 |
| `target/`、`node_modules/`、`dist/`、`venv/` | 数 GB | 可重新生成的依赖与编译产物 |
| `vendor/` | 654 MB | 外部资产的只读隔离副本，属第三方内容 |
| `data/webview2/` | — | **WebView2 浏览器用户数据**，含浏览历史、Cookie、登录数据 |
| `data/registry.db`、`search-index/` | 6 MB | 运行时数据库与索引 |
| `data/asset-research.json` | 667 KB | 本机能力扫描结果，含大量本机路径 |
| `public/preview.json` | 1.2 MB | 扫描结果快照（生成物） |
| `package-lock.json` | 120 KB | 可由 `npm install` 重新生成 |
| `*.bak` 备份文件 | 157 KB | 编辑器 / 重命名留下的备份 |
| 图标 png/ico（保留 1 个） | 236 KB | 打包资源，非源码 |
| `kernel/--help` | 2.3 MB | 被误命名的 SQLite 数据库文件 |
| `*.exe`、`*.dll`、`*.pdb` | — | 可执行产物 |

> **保留了什么没有丢**：所有 `.rs` / `.tsx` / `.ts` / `.py` 源码、协议定义、五类文档、
> 构建脚本、示例资产。功能描述、技术栈、架构设计全部完整可读。

---

## 脱敏说明

| 类型 | 处理 |
| --- | --- |
| 本机绝对路径 | 替换为中性路径（`C:/work/capability-core`） |
| 外部资产目录真实位置 | 替换为 `C:/external-assets`，保留「外部只读目录」语义 |
| 用户名目录 | 替换为 `C:/Builds/PublicUser01` |
| 测试假密钥 | 改为明显不可用的占位串 |
| 旧品牌名（中英文、环境变量前缀、Rust 类型名） | 统一替换为现产品名 |
| 浏览器痕迹 / 运行时数据库 | 整块剥离，未包含 |

**复扫结果**：旧项目名（中英文及环境变量前缀）、本机绝对路径、用户名目录、
常见个人邮箱域、密钥形态、第三方平台首页、手机号 —— **全部 0 命中**。
（扫描规则本身不在本文列举，避免把待检词写进交付物。）

**刻意保留的**：
- 第三方上游包的作者署名与版权声明（许可证要求，删了违规）
- 测试代码里的虚构路径（`D:/x/registry.db` 这类，明显是占位）

---

## 这份材料还有什么用

如果你要改进现在的 Capability Core，这个目录里最有价值的是三处：

1. **`kernel/crates/ac-core/`** —— 协议层数据模型。零 IO、零依赖，是整个仓库里
   最早被认真设计过的部分；现在的 Manifest 模型可以追溯到它。
2. **`docs/lessons/`** —— 踩坑记录。Tauri 的 dev/protocol 陷阱、manifest 嵌入、
   路径解析的坑，换栈之后依然成立。
3. **`docs/planning/03-里程碑设计存档-M2至M6.md`** —— 六层架构的原始推演。

除此之外没有多少值得沿用的东西。**新功能请基于现行代码，不要回到这里。**

---

## 这个目录可以被替换或删除

它是一份**历史快照**，不是活代码。开源社区维护者如果认为：

- 它的信息已经被现行代码和文档覆盖
- 它阻碍了仓库结构
- 或者有更好的方式保存这段历史（比如只在 Git 历史里保留）

**可以直接删除整个 `old/` 目录**，不需要征得原作者同意。
这一点在仓库根目录的 `HANDOVER.md` 里也有交代。
