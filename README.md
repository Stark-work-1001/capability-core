# 能力核心 · Capability Core

> 跨智能体的能力资产中枢：把散落在各个 AI 智能体里的技能、MCP 服务与插件统一采集进本地数据库，
> 让任何接入的智能体都能**主动检索、读取并调用**；同时持有自己的能力资产，承担故障诊断、重复裁决、
> 维修派发与模型增强检索。

版本 **1.0.0** · Windows 桌面应用 · 数据全部留在本机

---

## 一键开始

```text
双击 release/启动-能力核心.cmd
```

或直接双击：

```text
release/runtime/CapabilityCore.exe
```

首次启动会在后台扫描本机全部能力来源（约 20–30 秒），界面顶部有扫描进度提示。
数据保存在 `release/capability-core-data/`，能力资产保存在 `release/assets/`，均可整体备份或迁移。

开发模式：

```bash
npm run dev        # Vite dev server + Electron 热更新
npm test           # 35 项后端测试
npm run typecheck  # TypeScript 类型检查
npm run build      # 生成 release/ 绿色版
```

---

## 核心概念：能力的三分

能力核心里有三个容易混淆、但必须分清的东西：

| 概念 | 物理位置 | 数据库 | 含义 |
| --- | --- | --- | --- |
| **能力资产库** | `assets/<kind>/<id>/` | `capabilities.scope='own'` | 能力核心**自己持有**、可维修、可派发、可直接提供给接入智能体的能力实体 |
| **宿主能力库** | 各宿主目录（`~/.dsh`、`~/.codex` 等） | `capabilities.scope='host'` | 从已安装智能体**扫描登记**的能力索引，供检索与转化 |
| **能力数据库** | `data/capability-core.db` | 全部表 | 上述两者的元数据、状态、维修记录、向量与事件 |

**转化关系**

```
宿主能力库  ──「转为能力资产」──▶  能力资产库
            （复制实体到 assets/，登记来源，保留追溯）

能力资产库  ──「生态同步」──────▶  各智能体原生配置
            （写回宿主，让智能体直接可用）
```

区别一句话：**宿主能力库是"我看到了什么"，能力资产库是"我拥有什么"**。
前者只读索引，后者可维修、可派发、可发布给别的智能体使用。

---

## 它解决什么问题

| 现状 | 能力核心的做法 |
| --- | --- |
| 技能散落在 `~/.dsh`、`~/.codex`、`~/.minimax`、`D:/.agent-core` 各处 | 统一扫描 → 归一化为「能力」入 SQLite |
| 每个智能体都要重复安装同一份技能 | 一份纳管副本，多个宿主共享检索 |
| 不知道能力是谁写的、什么版本、还能不能用 | 作者 / 版本 / 健康状态 / 更新时间全部可见 |
| 坏了只能靠人翻目录排查 | 规则化诊断 + 自动修复 + 派单给智能体修 |
| 同一能力装了多份，版本不一致 | 去重裁决：保留最新版本 / 合并 / 共存 / 忽略 |
| 换智能体就得重新配一遍 MCP | 生态同步：一键写入宿主原生配置，可回滚 |
| 纯关键词检索，中文短语命中差 | 云端向量检索 + 精排，云端不可用时自动退回本地算法 |

---

## 六项核心能力

### 1. 多协议采集

| 协议 | 识别内容 |
| --- | --- |
| Agent Skills | `SKILL.md` + YAML frontmatter、资源文件清单 |
| MCP | `mcp.json` / `.mcp.json` / `config.toml` / `config.yaml` 的服务器声明；工程目录（package.json / pyproject.toml） |
| DSH 插件 | `package.json` + `dsh.bundle` / `dsh.client` |
| Claude / Codex 插件 | `plugin.json` / `.claude-plugin` / `.codex-plugin` |
| A2A | Agent Card（skills 声明） |
| OpenAPI 3 | `openapi.json` 操作声明 |

### 2. 混合检索

四路融合，全部可离线兜底：

- **FTS5 全文**（BM25，字段加权：名称 8 / 摘要 4 / 正文 1 / 标签 2 / 作者 2 / 宿主 1.5）
- **trigram 子串**（中文短语、路径片段）
- **向量检索**（硅基流动 `Qwen/Qwen3-VL-Embedding-8B`，4096 维）
- **精排**（`Qwen/Qwen3-VL-Reranker-8B`）

云端不可用时自动降级为 FTS5 + trigram + BM25，**任何情况下检索都不会中断**。

### 3. 能力资产库

- 新建 / 导入 / 从宿主能力转化
- 物理落盘 `assets/<kind>/<id>/`，与数据库双重登记
- 删除进 `assets/.trash`，记录归档可追溯
- 可通过 MCP 直接提供给接入的智能体（`capability.assets_list` / `capability.assets_read`）

### 4. 故障诊断与维修派发

中枢**只负责汇报并定位故障**，维修由接入模型的智能体来做。

- 规则化诊断：缺 frontmatter、路径失效、MCP 命令不存在、版本漂移等
- 安全自动修复：补 frontmatter / 补 description，修复前自动备份、可回滚
- 派单：生成结构化工单，智能体修完通过 `capability.submit_patch` 回传，中枢校验路径边界后落盘

### 5. 智能体接入

三条独立通道叠加，互为补充：

| 通道 | 机制 | 成本 |
| --- | --- | --- |
| A · MCP 端点注册 | 智能体连入 `/mcp` 后自动登记，可查谁在线、谁拉取了什么 | 中 |
| B · 共享目录心跳 | 各宿主写状态文件，中枢扫描 | 低 |
| C · 静态登记 | 记录能力属于哪个软件生态 | 最低 |

界面上每张智能体卡片提供两个按钮：

- **连通检测**（只读）：探测 home / config / process / heartbeat / session / endpoint 六项，返回时延与逐项结果，**不写任何文件**
- **主动连通**：把 MCP 配置写入宿主原生配置文件（写前自动备份），让该智能体立刻能获取能力

### 6. 模型接入

内置硅基流动（OpenAI 兼容协议），四种角色解耦：

| 角色 | 默认模型 | 用途 |
| --- | --- | --- |
| `embedding` | `Qwen/Qwen3-VL-Embedding-8B` | 向量检索，输出 4096 维 |
| `rerank` | `Qwen/Qwen3-VL-Reranker-8B` | 精排，提升命中顺序 |
| `judge` | `Qwen/Qwen3.8-27B` | 通用判定层 |
| `general` | `Qwen/Qwen3.8-27B` | 通用替代模型，兜底 |

- 提供商可增删改，**任意 OpenAI 兼容服务都可接入**（自定义 base_url / key / 请求头）
- 角色与型号解耦，界面上可随时改
- API Key 只存本地 SQLite，界面恒为打码显示（前 6 + `••••` + 后 4）
- 云端失败自动退回本地算法，并计入 `model_calls` 统计

**配置 API Key（必读）**

源码里**不含任何密钥**。两种方式任选其一：

1. **界面配置（推荐）**：启动后进入「模型接入」→「编辑」→ 填入你自己的 Key → 保存。
   密钥只写入本地数据库 `capability-core-data/capability-core.db`，不会进仓库、不会出现在日志。
2. **环境变量**：启动前设置

   ```powershell
   $env:CC_MODEL_API_KEY = "<你的 key>"
   $env:CC_MODEL_BASE_URL = "https://api.siliconflow.cn/v1"   # 可选，默认即此地址
   ```

不配置 Key 也能正常使用——检索会走本地的 FTS5 + trigram + BM25，精排与判定走本地算法，
只是没有云端语义增强。

---

## 侧栏结构

```
能力中枢
  总览              中枢运行状态与最近动态
  能力资产库        自有能力实体的创建、查看与转化
  宿主能力库        从各智能体扫描登记的技能 / MCP / 插件
  检索实验室        测试智能体如何在中枢内寻找能力
  智能体接入        进程识别、连通检测、主动连通与 MCP 会话
治理
  来源与扫描        能力从哪些目录被采集
  去重裁决          识别重复能力并保留最新版本
  维修站            故障诊断、自动修复与派单
  更新管理          上游新版本检查与快照更新
运行
  工作流            可观察、可复跑的维护流程
  模型接入          云端模型提供商、角色路由与调用统计
  生态同步          把中枢能力推送到各智能体原生配置
  数据库            SQLite 运行指标与表结构
  设置              中枢行为与连接信息
```

---

## MCP 工具

接入的智能体可调用以下工具（端点 `http://127.0.0.1:18297/mcp`）：

| 工具 | 用途 |
| --- | --- |
| `capability.search` | 按自然语言检索能力 |
| `capability.list` | 列出能力（可按类型/宿主/状态过滤） |
| `capability.get` | 取能力元数据 |
| `capability.read_skill` | 读取能力正文 |
| `capability.report_usage` | 上报使用，影响排序 |
| `capability.list_faults` | 查看维修工单 |
| `capability.submit_patch` | 回传修复补丁 |
| `capability.register_agent` | 注册当前智能体 |
| `capability.registry_stats` | 中枢统计 |
| `capability.sync_status` | 生态同步状态 |
| `capability.assets_list` | 列出能力资产库条目 |
| `capability.assets_read` | 读取能力资产正文 |
| `capability.model_status` | 查询模型接入可用性（不含密钥） |

---

## 目录结构

```
capability-core-1017/
  assets/                能力资产库（自有）
    skill/<id>/SKILL.md
    mcp/<id>/mcp.json
    plugin/<id>/
    prompt/<id>/
    .trash/              删除回收站
  data/                  开发态运行数据
    capability-core.db
    logs/ snapshots/ search/ backups/ exports/ run/
  apps/desktop/          Electron 主进程 + 渲染层
  scripts/               build / dev / start
  tests/                 后端测试
  docs/                  文档
  release/               npm run build 产物
    runtime/CapabilityCore.exe        启动器（双击即用）
    runtime/CapabilityCore-GUI.exe    Electron 本体
    runtime/resources/app/            应用代码
    capability-core-data/             打包态数据
    assets/                           打包态能力资产
    启动-能力核心.cmd
```

---

## 环境变量

| 变量 | 作用 |
| --- | --- |
| `CC_HOME` | 数据根目录（默认开发态 `data/`，打包态 `capability-core-data/`） |
| `CC_DB` | 数据库路径 |
| `CC_ASSET_DIR` | 能力资产库目录 |
| `CC_PORT` | 中枢 HTTP/MCP 端口（默认 18297） |
| `CC_PACKAGED` | 标记打包态（启动器自动设置） |
| `CC_DEV` | 开发模式（连 Vite dev server） |

---

## 测试

```bash
npm test          # 35 项，覆盖适配器 / 核心算法 / 服务 / HTTP+MCP / 模型 / 资产
npm run typecheck # TypeScript 类型检查
```
