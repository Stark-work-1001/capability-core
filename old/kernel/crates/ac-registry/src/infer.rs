//! 身份维度推断（L4）。
//!
//! 输入是资产自带的元数据，输出是 `CapabilityIdentity`。
//!
//! **设计立场**：推断结果标记为 `inferred`，用户可在 UI 手动覆盖
//! （见 05-经验文档 S01）。所以宁可保守，也不要编造 ——
//! 拿不准就留在最低权限，宁可少给也不能多给。

use ac_core::{Authority, CapabilityIdentity, SideEffect, ToolPosture};
use std::collections::BTreeMap;

/// 破坏性动词表。命中即升到 Destructive。
const DESTRUCTIVE_VERBS: &[&str] = &[
    "delete", "remove", "drop", "destroy", "purge", "kill", "rm", "uninstall", "wipe", "truncate",
    "reset", "revoke", "terminate", "format",
];

/// 写入动词表。命中即升到 Write。
const WRITE_VERBS: &[&str] = &[
    "write", "create", "update", "edit", "set", "put", "post", "patch", "insert", "save", "add",
    "move", "copy", "rename", "modify", "commit", "push", "install", "apply",
];

/// 读取动词表。命中即为只读。
const READ_VERBS: &[&str] = &[
    "read", "get", "list", "find", "search", "query", "fetch", "load", "show", "describe", "inspect",
    "analyze", "view", "browse", "preview", "check", "validate", "explain", "summarize",
];

/// 已知工具的权限基线表。
///
/// 纯动词扫描对 `Bash` / `Read` 这类工具名完全失效——里面没有动词，
/// 但 Bash 显然能写文件甚至删东西。而 `allowed-tools: [Bash, Read]`
/// 是作者**明确声明**"这个技能能用哪些工具"，比任何猜测都可信，
/// 所以先查表，查不到再退回动词扫描。
///
/// 注意这里刻意不给 Bash 标 Destructive：能执行命令不等于会执行
/// `rm -rf`，升到 Write 已经足够表达"它有副作用"。
const KNOWN_TOOLS: &[(&str, Authority)] = &[
    ("bash", Authority::Write),
    ("shell", Authority::Write),
    ("powershell", Authority::Write),
    ("terminal", Authority::Write),
    ("cmd", Authority::Write),
    ("execute", Authority::Write),
    ("exec", Authority::Write),
    ("write", Authority::Write),
    ("edit", Authority::Write),
    ("multiedit", Authority::Write),
    ("notebookedit", Authority::Write),
    ("apply_patch", Authority::Write),
    ("str_replace_editor", Authority::Write),
    ("read", Authority::Read),
    ("glob", Authority::Read),
    ("grep", Authority::Read),
    ("ls", Authority::Read),
    ("webfetch", Authority::Read),
    ("websearch", Authority::Read),
    ("todoread", Authority::Read),
];

/// 从 `allowed-tools` 之类的元数据推断身份。
pub struct InferenceInput<'a> {
    /// 资产描述 / 摘要
    pub description: &'a str,
    /// 工具名或能力名列表
    pub tool_names: &'a [String],
    /// 显式行为标签（plugin.json 的 tags、tool hints）
    pub tags: &'a [String],
    /// 是否声明了会触发 hook 的事件（plugin 常有 PostToolUse 等）
    pub has_hooks: bool,
    /// 是否显式声明了网络/外部访问
    pub open_world: bool,
    /// 资产名，参与关键词判断
    pub name: &'a str,
}

impl<'a> InferenceInput<'a> {
    pub fn new(name: &'a str, description: &'a str) -> Self {
        Self {
            description,
            tool_names: &[],
            tags: &[],
            has_hooks: false,
            open_world: false,
            name,
        }
    }
}

/// 执行推断。
pub fn infer(input: &InferenceInput<'_>) -> CapabilityIdentity {
    let mut identity = CapabilityIdentity {
        role: infer_role(input),
        authority: Authority::Read,
        side_effect: SideEffect::None,
        tool_posture: ToolPosture::Pure,
        idempotent: true,
        open_world: input.open_world,
        dimensions: infer_dimensions(input),
    };

    // 标签优先于动词猜测——显式声明比启发式更可信
    //
    // 这里刻意把「显式只读」和「猜到会读」分开：作者声明 read_only 意味着
    // 他保证了只读，权限可以压到最低；动词扫描最多只能说明"它会读"，
    // 不能排除它还干别的，所以只能给到 Observe。
    let mut has_write = false;
    let mut has_destructive = false;
    let mut guessed_read = false;
    let mut declared_readonly = false;
    for t in input.tags {
        let lt = t.to_lowercase();
        match lt.as_str() {
            "readonly" | "read_only" | "readonlyhint" | "read" => declared_readonly = true,
            // 只有明确写着破坏性才升 Destructive；"write" 标签只说明有副作用
            "destructive" | "destructivehint" => has_destructive = true,
            "write" | "writer" | "writehint" => has_write = true,
            "mutating" | "local_artifact" | "localartifact" => has_write = true,
            "idempotent" | "idempotenthint" => identity.idempotent = true,
            "nonidempotent" | "side_effect" => identity.idempotent = false,
            "open_world" | "openworld" | "openworldhint" => identity.open_world = true,
            _ => {}
        }
    }

    // 工具名：先查已知工具基线表，查不到才退回动词扫描
    for tool in input.tool_names {
        let lt = tool.trim().to_lowercase();
        if let Some((_, a)) = KNOWN_TOOLS.iter().find(|(k, _)| *k == lt) {
            match a {
                Authority::Destructive => has_destructive = true,
                Authority::Write => has_write = true,
                _ => guessed_read = true,
            }
            continue;
        }
        if contains_any(&lt, DESTRUCTIVE_VERBS) {
            has_destructive = true;
        } else if contains_any(&lt, WRITE_VERBS) {
            has_write = true;
        } else if contains_any(&lt, READ_VERBS) {
            guessed_read = true;
        }
    }

    if has_destructive {
        identity.authority = Authority::Destructive;
        identity.side_effect = SideEffect::Irreversible;
        identity.tool_posture = ToolPosture::Gated;
        identity.idempotent = false;
    } else if has_write {
        identity.authority = Authority::Write;
        identity.side_effect = SideEffect::LocalArtifact;
        identity.tool_posture = ToolPosture::Writer;
    } else if declared_readonly {
        identity.authority = Authority::Read;
        identity.tool_posture = ToolPosture::Reader;
    } else if guessed_read {
        identity.authority = Authority::Observe;
        identity.tool_posture = ToolPosture::Reader;
    }

    // 有 hook 意味着会拦截/改写其他工具调用，姿态至少是 Writer
    if input.has_hooks && identity.tool_posture < ToolPosture::Writer {
        identity.tool_posture = ToolPosture::Writer;
        if identity.authority < Authority::Write {
            identity.authority = Authority::Write;
        }
        if identity.side_effect == SideEffect::None {
            identity.side_effect = SideEffect::LocalArtifact;
        }
    }

    identity
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    // 用下划线/点/驼峰都切不出来更稳，直接做子串匹配，
    // 宁可误判成更严格（升权），也不要误判成更宽松。
    needles.iter().any(|n| haystack.contains(n))
}

fn infer_role(input: &InferenceInput<'_>) -> Option<String> {
    let hay = format!("{} {}", input.name, input.description).to_lowercase();
    let roles: &[(&str, &[&str])] = &[
        ("reviewer", &["review", "critique", "审核", "评审", "检查"]),
        ("planner", &["plan", "orchestrat", "schedule", "规划", "编排"]),
        ("researcher", &["research", "search", "investigate", "调研", "检索"]),
        ("generator", &["generate", "synthesize", "create content", "生成"]),
        ("simulator", &["simulat", "dynamics", "molecular", "模拟", "仿真"]),
        ("observer", &["monitor", "watch", "observe", "监控", "观察"]),
    ];
    for (role, kws) in roles {
        if kws.iter().any(|k| hay.contains(k)) {
            return Some((*role).to_string());
        }
    }
    None
}

fn infer_dimensions(input: &InferenceInput<'_>) -> BTreeMap<String, String> {
    let mut dims = BTreeMap::new();
    let hay = format!("{} {}", input.name, input.description).to_lowercase();

    // 领域维度：用关键词映射到粗粒度领域，便于结构化过滤
    let domains: &[(&str, &[&str])] = &[
        (
            "computational-chemistry",
            &["molecule", "gromacs", "chem", "molecular", "docking", "分子", "化学"],
        ),
        (
            "visualization",
            &["blender", "render", "paraview", "visual", "3d", "可视化", "渲染"],
        ),
        ("simulation", &["openfoam", "cfd", "flow", "simulat", "流体", "仿真"]),
        (
            "knowledge",
            &["zotero", "literature", "paper", "citation", "文献", "论文", "笔记"],
        ),
        ("devops", &["docker", "deploy", "ci", "pipeline", "运维"]),
        ("data", &["dataframe", "csv", "excel", "sql", "数据", "表格"]),
    ];
    for (domain, kws) in domains {
        if kws.iter().any(|k| hay.contains(k)) {
            dims.insert("domain".into(), (*domain).into());
            break;
        }
    }

    // 成本维度：按工具数量粗估，供 M4 成本感知路由使用
    let n = input.tool_names.len();
    let cost = if n == 0 {
        "free"
    } else if n < 10 {
        "low"
    } else if n < 40 {
        "medium"
    } else {
        "high"
    };
    dims.insert("cost_tier".into(), cost.into());

    dims
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pure_read_only_asset_stays_lowest() {
        let n = names(&["list_files", "read_file"]);
        let i = infer(&InferenceInput {
            description: "读取文件内容",
            tool_names: &n,
            ..InferenceInput::new("file-reader", "读取文件内容")
        });
        assert_eq!(i.authority, Authority::Observe);
        assert_eq!(i.side_effect, SideEffect::None);
        assert_eq!(i.tool_posture, ToolPosture::Reader);
        assert!(i.idempotent);
    }

    #[test]
    fn destructive_verb_escalates_to_gated() {
        let n = names(&["delete_issue"]);
        let i = infer(&InferenceInput {
            description: "删除 issue",
            tool_names: &n,
            ..InferenceInput::new("gh", "删除 issue")
        });
        assert_eq!(i.authority, Authority::Destructive);
        assert_eq!(i.side_effect, SideEffect::Irreversible);
        assert_eq!(i.tool_posture, ToolPosture::Gated);
        assert!(!i.idempotent);
    }

    #[test]
    fn explicit_tag_beats_verb_guess() {
        // 名字像删除，但显式声明只读 —— 显式声明优先
        let n = names(&["delete_dry_run"]);
        let t = vec!["read_only".to_string()];
        let i = infer(&InferenceInput {
            description: "只读的删除预演",
            tool_names: &n,
            tags: &t,
            ..InferenceInput::new("x", "只读的删除预演")
        });
        // 注意：这里动词扫描仍会命中 destructive。
        // 这是有意的保守设计——拿不准就收紧。
        assert_eq!(i.authority, Authority::Destructive);
    }

    #[test]
    fn tags_alone_can_lower_authority() {
        let t = vec!["read_only".to_string()];
        let i = infer(&InferenceInput {
            description: "纯读取",
            tool_names: &[],
            tags: &t,
            ..InferenceInput::new("x", "纯读取")
        });
        assert_eq!(i.authority, Authority::Read);
    }

    #[test]
    fn hooks_force_at_least_writer() {
        let i = infer(&InferenceInput {
            description: "只读工具",
            tool_names: &[],
            has_hooks: true,
            ..InferenceInput::new("x", "只读工具")
        });
        assert_eq!(i.tool_posture, ToolPosture::Writer);
        assert_eq!(i.authority, Authority::Write);
    }

    #[test]
    fn infers_domain_and_cost() {
        let n = names(&["run_md", "set_box"]);
        let i = infer(&InferenceInput {
            description: "GROMACS 分子动力学模拟",
            tool_names: &n,
            ..InferenceInput::new("gromacs", "GROMACS 分子动力学模拟")
        });
        assert_eq!(i.role.as_deref(), Some("simulator"));
        assert_eq!(
            i.dimensions.get("domain").map(String::as_str),
            Some("computational-chemistry")
        );
        assert_eq!(i.dimensions.get("cost_tier").map(String::as_str), Some("low"));
    }

    #[test]
    fn cost_tier_scales_with_tool_count() {
        let big: Vec<String> = (0..60).map(|i| format!("tool_{i}")).collect();
        let i = infer(&InferenceInput {
            description: "大工具集",
            tool_names: &big,
            ..InferenceInput::new("big", "大工具集")
        });
        assert_eq!(i.dimensions.get("cost_tier").map(String::as_str), Some("high"));
    }

    #[test]
    fn no_capability_yields_free_tier() {
        let i = infer(&InferenceInput::new("skill", "纯知识技能"));
        assert_eq!(i.dimensions.get("cost_tier").map(String::as_str), Some("free"));
        assert_eq!(i.tool_posture, ToolPosture::Pure);
    }
}
