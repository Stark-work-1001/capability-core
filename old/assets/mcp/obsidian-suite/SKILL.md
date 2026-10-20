---
name: obsidian-agent
version: "1.1.0"
description: "Obsidian knowledge-base agent: eight skills plus two bundled MCP entries. Skill layer - obsidian-markdown (Obsidian Flavored Markdown: wikilinks, embeds, callouts, properties, block refs), obsidian-bases (.base views/filters/formulas/summaries), json-canvas (.canvas nodes/edges/groups), obsidian-cli (official CLI incl. plugin & theme dev loop), defuddle (clean markdown extraction from HTML), knap (template-driven note generation), obsidian-mcp-session (live session protocol and acceptance gates), obsidian-plugin-forensics (proving a self-written plugin actually runs in the app: metadata/build-fingerprint/view-binding/freshness gates and precondition-honest assertions). MCP layer - obsidian-rest (15 tools over the Local REST API community plugin: list/read/batch-read/search/simple-complex-tag/frontmatter/recent-changes/patch/append/put/delete/periodic-notes) and obsidian-cli (4 tools bridging the official Obsidian 1.12+ command line: detect/help/run/batch). A missing API key, a dead port, a closed app or a disabled CLI always returns ok:false with the exact remedy; nothing is reported as done that the vault does not show. Use for Obsidian vaults, notes, .md/.base/.canvas files, wikilinks, daily notes, vault search, plugin development, or packaging Obsidian work as a skills+MCP plugin."
capabilities:
  - MCP entry obsidian-rest - 15 tools over the Local REST API community plugin (https 27124 / http 27123), reads and writes vault files
  - MCP entry obsidian-cli - 4 tools bridging the official Obsidian 1.12+ CLI (Obsidian.com shim) against the running app, with a destructive-token gate
  - Skills - obsidian-markdown, obsidian-bases, json-canvas, obsidian-cli, defuddle, knap, obsidian-mcp-session, obsidian-plugin-forensics
  - Bundled interpreter - mcp/.venv pins mcp>=1.1,<2 so the upstream REST server runs untouched on a machine whose system Python carries SDK 2.x
triggers:
  - "Obsidian / vault / 知识库 / 笔记 / daily note"
  - "wikilink / backlink / callout / frontmatter / properties / embed"
  - ".base / Bases / table view / filters and formulas"
  - ".canvas / JSON Canvas / mindmap / whiteboard"
  - "Local REST API / OBSIDIAN_API_KEY / mcp-obsidian"
  - "obsidian CLI / plugin:reload / dev:errors / eval"
  - "defuddle / knap / template batch generation"
  - "自写插件开发 / 部署 / 自检 / 真机验证 / main.js 没生效 / build fingerprint / app:reload"
---

# Obsidian Agent（插件：技能层 + 双 MCP 条目）

## Overview

把「操作 Obsidian 知识库」这件事分成三条通道，按**代价从低到高**选：

1. **直接读写 vault 文件**（零依赖）——vault 只是磁盘上的 `.md`/`.base`/`.canvas`；本插件的 `obsidian-markdown` / `obsidian-bases` / `json-canvas` 三个技能给出语法规范。改文件即可，Obsidian 会自动重载。
2. **MCP `obsidian-cli` 条目**（4 工具）——桥接 Obsidian 1.12+ 官方命令行，直连**正在运行的应用**：能用应用的能力（活动文件、周期笔记、插件热重载、`dev:errors`、`eval`、截图），且不需要第三方插件，只要在设置里打开一个开关。
3. **MCP `obsidian-rest` 条目**（15 工具）——上游 `mcp-obsidian` 0.2.2 经 Local REST API 社区插件读写 vault：结构化最强（JsonLogic 复杂检索、按标签检索、frontmatter 读取、批量读、最近改动、相对标题/块引用的 patch）。代价是每个 vault 都要装并启用该社区插件、把 API key 填进配置。

选型只改变下一步动作时才评估；一句话能定的直接干（工作区价值闸门）。

## 通道选择表

| 需求 | 走哪条 | 理由 |
| --- | --- | --- |
| 写/改单篇笔记、批量生成文件、建 `.base`/`.canvas` | 文件直读直写 + 对应技能 | 无依赖、可 diff、可回滚；Obsidian 自动感知 |
| 需要「当前活动文件」「周期笔记」等**应用态** | `obsidian-cli` 条目 | REST 通道看不到 UI 状态 |
| 需要 Obsidian 的检索语义（反链、标签计数、JsonLogic 查询） | `obsidian-rest` 条目 | 这些只有 REST API 暴露 |
| 周期笔记 / 最近改动 | `obsidian-cli`（`daily:path`、`daily:read`、`daily:append`、`files`）| 当前 Local REST API 5.1.0 不提供 `/periodic/…`，DQL 扩展也未注册（缺口 G1/G2，见 `mcp\PLUGIN-NOTES.md`）——**404 不等于「库里没有周期笔记」** |
| 开发/调试 Obsidian 插件与主题 | `obsidian-cli` 条目（`plugin:reload`、`dev:errors`、`dev:screenshot`、`eval`）+ `obsidian-cli` 技能 | 热重载与错误捕获必须在应用内 |
| 网页正文入库 | `defuddle` 技能（CLI 未装时退化为 WebFetch，并说明差异） | 抽正文省 token |
| 由结构化数据批量出笔记 | `knap` 技能 | 模板 + 过滤 + 循环 |

## 硬规则

1. **不得把「工具返回」说成「知识库已改」**。写入类调用之后必须回读（`obsidian_get_file_contents` 或 `obsidian_cli_run(["read", ...])`，或直接 Read vault 文件）并引用回读内容作证据。`obsidian-rest` 的 `put/append/patch` 返回的是纯文本 "Successfully ..."——那只表示 REST 层没报错。
2. **`ok:false` 分两种，别混着说**：`not-configured`/`connection-refused` 是**通道没通**；`backend-endpoint-missing`(40400)/`backend-extension-missing`(40012) 是**该后端版本不提供这条能力**——后者要如实说「这个版本没有这个端点/扩展」，不能讲成「你的库里没有周期笔记/没有改动」。缺口清单见 `mcp\PLUGIN-NOTES.md`（G1 最近改动、G2 周期笔记，均给了替代路径）。
3. **`ok:false` 不是失败，是通道没通**。看 `remedy` 字段按步补前置（`references/blocker-remediation.md`），不要静默改用另一条通道后当作原方案验证过。
4. **删除必须点名并先征得同意**。`obsidian_delete_file` 需 `confirm=true`（上游自带闸门），`obsidian_cli_run` 对 `delete|remove|uninstall|disable|eval` 令牌要求 `confirm=true` 否则不 spawn。vault 属于用户，先展示路径再动手。
5. **两条通道的目标 vault 可能不同**。`obsidian-cli` 默认打到「最近聚焦的 vault」，跨 vault 必须显式 `vault=<名字>`；`obsidian-rest` **没有 vault 参数**——它打到「谁占住了 `OBSIDIAN_PORT` 的那个库」，多库同端口时只有一个能绑，其余静默不服务。本机 2026-09-21 17:4x 实测：`D:\001-run` 与 `D:\003-knowledge` 都装了该插件、**两份 `data.json` 的 key 与 `mcp\.env` 完全同值**，而 27124 由 `D:\001-run` 应答、27123 无人监听 → **认证通过不代表连对库**。写之前必须做一次归属确认（`GET /vault/` 根清单 vs 磁盘清单比对，配方见 `skills\obsidian-mcp-session\SKILL.md` §1 与 docs\evidence\host-registration-2026-09-21.md）。多库场景要定向，就走 CLI 条目的 `vault=`。
   - **2026-09-25 起本机是双实例并存**：`27123/27124` = 主力库那套（`D:-run`、`D:-knowledge`），`27125`（http）/`27126`（https）= 沙箱取证库 `D:\obsidian-link-test` 专用（3D 导图插件的 `verify_live.mjs` 走这一条，key 也不同）。**两套的 key 不通用**，连错端口的表现是 401/403 而不是 refused —— 所以定向的唯一办法是先 `GET /` 读 `versions` 再 `GET /vault/` 核归属，别按端口号猜。
6. **`eval` 是任意代码执行**，跑在用户的应用进程里；只在用户明确要求插件调试时提议，并说明会改动运行态。
7. **配置改完要重启条目**。host/port/protocol 与 API key 都在服务端 import 期读一次（上游实现如此），改 `.env` 或清单 `env` 后必须重载插件或重启宿主。
8. 结论必须有事实依据：文档里凡「未验证」的项都写明**为什么没做**（能测未测 vs 本机不可测），见 `docs/mcp-integration.md` §6。

## 上手 60 秒

```
mcp\.venv\Scripts\python.exe mcp\check_env.py      # 现在哪条通道能用、缺什么
mcp\.venv\Scripts\python.exe tests\selftest.py     # 离线回归：两条目握手 + 反伪成功闸门
mcp\.venv\Scripts\python.exe tests\selftest.py --upstream   # 外加上游 81 项测试
```

不经宿主插件加载也能跑：`mcp\run_server.cmd rest|cli|check|selftest`。

## 目录

```
obsidian-agent\
├── SKILL.md                    # 本文件：通道选择 + 硬规则 + 索引
├── README.md                   # 组成、启用、前置条件、验证状态、版本谱系
├── CHANGELOG.md                # 1.0.0 整合记录
├── ACKNOWLEDGEMENTS.md         # 上游来源与许可边界
├── LICENSE                     # 本作代码与文档（MIT）；上游各留自己的 LICENSE
├── .qoder-plugin\plugin.json   # Qoder 清单   ┐
├── mcp.json                    # Qoder MCP 启动 ┤ 加载 skills/ 与两个 MCP 条目
├── .zcode-plugin\plugin.json   # ZCode 清单   ┘
├── agents\openai.yaml          # 宿主角色入口定义
├── skills\                     # 7 个技能（6 个上游 + 本作的实时会话技能）
├── mcp\
│   ├── server_entry.py         # REST 条目：落位 + SDK 闸门 + 未配置时给 remedy
│   ├── obsidian_cli_server.py  # CLI 条目：真 subprocess 桥接官方 CLI
│   ├── check_env.py            # 只读体检（--json）
│   ├── PLUGIN-NOTES.md         # 上游来源 / 本地补丁 #1 / 已知缺口 G1·G2
│   ├── requirements.txt        # mcp>=1.1,<2 等，与 setup_env.cmd 一致
│   ├── setup_env.cmd           # 重建 .venv（ASCII + CRLF）
│   ├── run_server.cmd          # 手启/自检薄壳（ASCII + CRLF）
│   ├── src\mcp_obsidian\       # 上游 0.2.2 + 一处本地补丁（见 PLUGIN-NOTES.md）
│   ├── tests\                  # 上游 pytest（81 项）
│   └── .venv\                  # 随包解释器（换机器用 setup_env.cmd 重建）
├── tests\selftest.py           # 离线回归（协议 + 工具清单 + 反伪成功）
├── references\                 # 本机实测 / 阻塞处置 / 工具速查
├── docs\                       # mcp-integration.md（决策与验证台账）+ evidence\（日志与 tools/list 存档）
├── 05_待办事项与问题记录.md      # 工作区审计文档：待办 + 问题账（含宿主接入现状）
└── 10_手动复现操作步骤.md        # 工作区审计文档：人工复现手册（环境版本 → 落位 → 验收）
```

## 相关记忆与工作区位置

交付路径 `D:\.my-agent\.plugin\obsidian-agent\`（2026-09-21 由 `D:\001-run\` 迁入 `.plugin\`，与 abaqus / ansys / comsol / solidworks 同目录，因此工作区的 `sync_plugins.py` / `register_qoder.py` 能直接扫到）。同类整合约定与坑位见同目录下其它 `*-agent` 插件。
