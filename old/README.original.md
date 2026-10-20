# capability-core

> 桌面端智能体能力中枢 —— 让智能体不用折腾装插件，直接在中枢里检索并调用能力。
> 项目代号 **Capability Nexus** · 版本 v0.1.0

---

## 快速开始

```bash
# 1. Rust 内核
cd kernel && cargo build --release && cargo test --release

# 2. 桌面应用（M0 完成后可用）
cd ../apps/desktop && npm install && npm run tauri dev
```

---

## 项目结构

| 路径 | 内容 |
|---|---|
| `kernel/` | Rust workspace，6 个 crate |
| `apps/desktop/` | Tauri 2 桌面应用 |
| `proto/` | Capability Manifest v1 协议定义 |
| `vendor/` | 外部资产隔离副本（**只读**，源目录零改动） |
| `data/` | SQLite + 倒排索引 + 向量索引 |
| `docs/` | 五类文档 |
| `artifacts/` | 打包产物与测试报告 |

---

## 六层架构

```
L6 套件/工作台      Suite & Workbench        ▲ 定义
L5 组织（智能体编队）Organization & Delegation ▲ 定义  ← 全网空白
L4 身份（执行身份）  Capability Identity      ▲ 定义  ← 全网空白
L3 检索与路由        Retrieval & Routing      ▲ 核心
L2 资产注册          Registry & Packaging     ▲ 核心
L1 连接与执行        MCP Protocol             ○ 适配
```

---

## 模块解耦纪律

```
ac-core（协议层，零 IO、零依赖）
   ▲  ▲  ▲  ▲  ▲
   │  │  │  │  └── ac-ipc        类型转换
   │  │  │  └───── ac-mcp         MCP 服务端 + 多智能体
   │  │  └──────── ac-llm         叶子节点
   │  └─────────── ac-retrieval    经 trait 抽象存储
   └────────────── ac-registry
```

**硬性约束**：`ac-retrieval` 不直接依赖 `ac-registry`（通过 trait），
这样检索算法可脱离 SQLite 单独测试。

---

## 文档

| 文档 | 内容 |
|---|---|
| [00 总览](docs/00-总览.md) | 项目定位、架构、选型 |
| [01 调研文档](docs/research/01-调研文档.md) | 借鉴了哪些开源项目的哪些部分 |
| [02 规划文档](docs/planning/02-规划文档.md) | 里程碑、完成标志、功能链路 |
| [03 改动文档](docs/changelog/03-改动文档.md) | 版本记录与全部改动 |
| [04 测试文档](docs/testing/04-测试文档.md) | 测试项、指标、结果 |
| [05 经验文档](docs/lessons/05-经验文档.md) | 踩坑与**提升建议** |

---

## 重要约束

> **本项目不得影响 `C:\external-assets\` 下其他插件的运行。**

`vendor/` 是隔离副本，所有测试只在副本上进行。源目录只读。