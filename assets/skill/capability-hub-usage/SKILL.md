---
name: capability-hub-usage
description: 在能力核心中检索并调用能力的工作流说明
version: 1.0.0
author: 能力核心
license: MIT
tags: [meta, usage, hub]
---

# 能力中枢使用说明

当你不确定本机有哪些可用能力时，按下面的顺序操作。

## 1. 检索

调用 MCP 工具 `capability.search`：

```json
{ "query": "读取 PDF 并提取文本", "limit": 5 }
```

返回每一行的 `id` 是后续调用的句柄。结果里同时给出作者、宿主、健康状态与匹配解释。

## 2. 读取正文

```json
{ "id": "cap_xxx" }
```

调用 `capability.read_skill` 拿到完整 SKILL.md。按其中的指令执行，不要凭名称猜测用法。

## 3. 上报使用

```json
{ "capabilityId": "cap_xxx", "action": "use" }
```

调用 `capability.report_usage`，中枢据此调整检索排序。

## 4. 遇到能力损坏

1. `capability.list_faults` 查看工单
2. 按工单里的路径修复文件
3. `capability.submit_patch` 回传完整文件内容与说明

中枢会校验路径边界、备份并记录维修历史。

## 5. 能力资产

`capability.assets_list` 列出能力核心自己持有的能力资产；
`capability.assets_read` 读取其中一条的完整内容。
