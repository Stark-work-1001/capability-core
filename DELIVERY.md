# 能力核心 · Capability Core —— 交付说明

版本 **1.0.0** · Windows 桌面应用 · 数据全部本地

---

## 一、怎么启动

**方式一（推荐）**

```
release/启动-能力核心.cmd
```

（完整路径：交付目录下的 `release\启动-能力核心.cmd`）

**方式二（直接双击 exe）**

```
release/runtime/CapabilityCore.exe
```

（完整路径：交付目录下的 `release\runtime\CapabilityCore.exe`）

两种方式等价。`CapabilityCore.exe` 是原生启动器（5.6 KB），负责清环境并拉起
`CapabilityCore-GUI.exe`（Electron 本体，234 MB）。**不要直接双击 GUI 本体**——
本机若存在全局 `ELECTRON_RUN_AS_NODE` 变量，GUI 会静默秒退。

---

## 二、数据在哪

| 内容 | 路径 |
| --- | --- |
| 运行数据 | `release/capability-core-data/` |
| 能力数据库 | `release/capability-core-data/capability-core.db` |
| 能力资产 | `release/assets/` |
| 日志 | `release/capability-core-data/logs/` |

三者都可整体备份或迁移。**升级时只覆盖 `release/runtime/`，不要覆盖上面三处。**

---

## 三、本次交付做了什么

### 1. 品牌与命名重构

产品名从旧名统一为 **能力核心 / Capability Core**。

- 中文产品名：能力核心
- 英文产品名：Capability Core
- npm 包名：`capability-core` / `capability-core-desktop`
- 环境变量前缀：`CC_`（原前缀已全部移除）
- 数据库：`capability-core.db`；资产目录：`assets/`；打包态数据：`capability-core-data/`
- MCP 工具：`capability.*`（13 个，原 10 个 + 新增 3 个）

术语全部回归使用场景：能力资产库、宿主能力库、能力数据库、注册、维修、派发、新增。

### 2. 能力的三分（本次最重要的概念澄清）

| 概念 | 位置 | 含义 |
| --- | --- | --- |
| 能力资产库 | `assets/<kind>/<id>/` + `scope='own'` | 自己持有、可维修、可派发 |
| 宿主能力库 | 各宿主目录 + `scope='host'` | 扫描登记的外部能力索引 |
| 能力数据库 | `data/capability-core.db` | 两者的元数据与状态 |

转化方向：宿主能力 ──「转为能力资产」──▶ 能力资产库 ──「生态同步」──▶ 各智能体配置。

### 3. 新增「能力资产库」侧栏

自有能力的创建、查看、转化、回收站。物理落盘 + 数据库双重登记，
删除进 `assets/.trash` 且记录归档可追溯。

### 4. 新增「模型接入」侧栏

内置硅基流动（OpenAI 兼容协议），四个角色解耦：

| 角色 | 默认模型 |
| --- | --- |
| embedding | `Qwen/Qwen3-VL-Embedding-8B`（4096 维） |
| rerank | `Qwen/Qwen3-VL-Reranker-8B` |
| judge | `Qwen/Qwen3.8-27B` |
| general | `Qwen/Qwen3.8-27B` |

- 任意 OpenAI 兼容提供商可增删改
- API Key 只存本地，界面恒打码
- 云端失败自动退回本地算法，检索永不中断

### 5. 智能体接入增加「连通检测」「主动连通」

- **连通检测**：只读探测六项（home/config/process/heartbeat/session/endpoint），返回时延与逐项结果
- **主动连通**：写入宿主原生 MCP 配置，写前自动备份，让智能体立刻能获取能力

### 6. 桌面程序启动方式

除原有 `.cmd` 外，新增独立 exe 启动方式（见第一节）。

---

## 四、验收记录（协调者实测）

| 项目 | 结果 |
| --- | --- |
| `npm test` | 35 / 35 通过 |
| `npm run typecheck` | 退出码 0 |
| `npm run build` | 退出码 0，连跑两次幂等 |
| 老库迁移 | 313 条能力 / 328 实例零丢失，schema 12→13，二次打开幂等 |
| 模型 embedding | 4096 维，云端实测 459 ms |
| 模型 rerank | 正常降序，366 ms |
| 模型 judge | 正常返回，1430 ms |
| 密钥保护 | 界面 `sk-rni••••qunn`，库内存完整值 |
| 智能体连通检测 | 6 项 checks，655 ms，未知宿主不抛错 |
| 资产目录名 == 主键 | 通过 |
| promote 去重 | 二次转化不新增记录 |
| 双击 exe 数据落点 | `release/capability-core-data` 正确 |
| 禁用词全仓复查 | 非第三方文件 0 命中 |

界面四屏（总览 / 模型接入 / 能力资产库 / 智能体接入）已逐一截图核对。

---

## 五、已知限制

1. **绿色版无安装程序**：无开始菜单项、无卸载器。升级即覆盖 `release/runtime/`。
2. **启动器未签名**：首次运行可能出现 SmartScreen「未知发布者」提示。
3. **构建需 .NET Framework**：`npm run build` 用 Windows 自带的 `csc.exe` 编译启动器。
   Windows 10/11 默认自带；若缺失构建会明确报错而非静默产出坏包。**运行时不需要 .NET**。
4. **智能体类型能力转化会降级**：资产只有 skill/mcp/plugin/prompt 四种物理形态，
   `kind='agent'` 的能力转为资产时降级为 `skill`，界面会明确标注。
5. **首次扫描耗时**：本机能力较多时约 20–30 秒，扫描在后台进行，不阻塞界面。

---

## 六、API Key 配置

本仓库**不含任何 API Key**。首次使用「模型接入」前需自行配置：

- **界面配置（推荐）**：「模型接入」→「编辑」→ 填入自己的 Key → 保存（只存本地数据库）
- **环境变量**：`CC_MODEL_API_KEY` / `CC_MODEL_BASE_URL`

不配置也能正常使用，检索会走本地 FTS5 + trigram + BM25，精排与判定走本地算法。

---

## 七、开发命令

```bash
npm run dev        # 开发模式（Vite + Electron 热更新）
npm test           # 35 项后端测试
npm run typecheck  # TypeScript 类型检查
npm run build      # 构建 release/ 绿色版
npm run build:web  # 仅构建渲染层
```
