//! 守门测试：**任何 example 都不得默认可写地打开用户的 registry.db**。
//!
//! # 这条纪律为什么存在
//!
//! M2 全文检索切片的第一版直接对用户的 data/registry.db 调了 Registry::open。
//! 而 Registry::open 会执行建表/迁移并**重写 SQLite 页** —— 打开本身就是一个
//! 写操作。后果是用户的库变成了「字节不同、内容完好」的状态。事故已修复，
//! 但纪律本身没有被强制：同一时刻 m2_e2e.rs 里还留着完全相同的写法，
//! 只是没人再跑它。任何人 cargo run --example m2_e2e 就会重演。
//!
//! 「写进文件头的警告」不是护栏：它只在有人**先打开文件**时才生效，
//! 而事故的成因恰恰是没有先打开文件。所以这条必须由测试强制。
//!
//! # 为什么是运行时读文件，而不是 include_str!
//!
//! 第一版用了 include_str!，实测**抓不到任何东西**：把 examples/m2_e2e.rs
//! 换回事故版本后重跑，cargo 输出 "Finished in 0.27s" —— 根本不重编，
//! 测试读到的是编译期嵌进去的**旧内容**，于是全绿。
//!
//! 根因：cargo 只在 crate 的 src/ 发生变化时重编 lib 目标，examples/ 下的
//! 文件不属于该目标的输入图。所以 include_str! 在这里既不报错也不更新，
//! 是比没有守门更糟的东西 —— 它给出一个绿色的假信号。
//!
//! 本文件因此在**运行时**用 std::fs::read_to_string 现读 examples/。
//! 代价只是几 KB 的文件 IO；换来的是「改了 example 就一定会被抓到」。
//!
//! # 设计原则（本轮返工的原因，改这里之前先读一遍）
//!
//! **守门规则必须与命名无关。一条只认某几个变量名的规则，是假的安全感。**
//!
//! 上一版红线是这么写的：
//!
//!     let opens_source = c.contains("db_path()") || c.contains("&path)")
//!         || c.contains("&real_db)") || c.contains("&db)");
//!
//! 四条**字面变量名**。评审实测：把事故版本的变量从 path 改名成 live，
//! 行为一模一样（默认 ../data/registry.db → Registry::open → 打开即写），
//! 守门测试**全绿**。一次改名就把「被抓到的缺陷」变成「静默绿灯」。
//! 更糟的是它在心理上比没有守门更危险：列表里再补一个名字，
//! 守门看起来更强大，而缺口只是往后挪了一格。
//!
//! 本版规则 2 改成**反向证明**（见下方 origin_of_expr / rule2_red_line_violations）：
//! 对每个 Registry::open 的实参，追它的数据流来源，必须能证明来自
//! **fs::copy 的落点 / temp_dir / 本地新建 scratch 目录**；
//! 追不到 → 判红。命令行实参单独处理（见下），它是「用户显式选的路径」，
//! 但**前提是这个文件没有静默默认到用户库** —— 因为事故形态里，
//! args 分支和默认分支赋给同一个变量，「用户显式选」根本区分不出来。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// examples/ 目录。
fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// 守门自检用的绕过样本目录（**不是** examples/，永远不会被 cargo 编译或运行）。
fn guard_testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata").join("db_safety_guard")
}

/// 运行时读一个 example 的源码。
///
/// 刻意**不**用 include_str!：见文件头「为什么是运行时读文件」。
fn read_example(name: &str) -> String {
    let p = examples_dir().join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {}: {e}", p.display()))
}

/// examples/ 下全部 .rs 文件名（已排序）。
fn example_names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(examples_dir())
        .unwrap_or_else(|e| panic!("读不到 examples/ 目录 {}: {e}", examples_dir().display()))
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".rs"))
        .collect();
    v.sort();
    v
}

/// 本文件显式审计的 example 清单。
///
/// 新增 example 必须登记进来，every_example_is_registered_in_the_guard 会拦。
const WATCHED: &[&str] = &[
    "ingest_vendor.rs",
    "m15_e2e.rs",
    "m2_e2e.rs",
    "m2_fulltext_e2e.rs",
    "m2_verify_db.rs",
    "migrate_check.rs",
    // M4：巡检例程。它**强制**要求显式 --db，没有默认值——
    // 正因为如此才要登记进来，让守卫核对它确实没有默认到用户库。
    "m4_sweep.rs",
    // 维修站诊断探针：同样是强制显式 --db。它只读诊断，但守门是文件名级的，
    // 登记后 rule2 会核对它确实没有把 data/registry.db 当兜底。
    "diag_probe.rs",
    // 数据层体检探针（2026-10-18）：块标量垃圾 / 去重队列 / 来源四列。
    // 它调 Registry::open（会迁移，即写库），所以**必须**进这份清单受 rule2 约束；
    // 单测 every_example_is_registered_in_the_guard 就是钉这一条的。
    "data_probe.rs",
    // v8 后端能力实测探针（2026-10-18）：宿主协议注册表 / 生命周期 / 作者 /
    // 资产↔宿主支持。它调 Registry::open（会迁移，即写库），且 seed_defaults
    // 会往 --data-dir/hosts/ 写清单，所以**必须**进这份清单受 rule2 约束。
    "hosts_probe.rs",
    // 模型分工路由表探针（2026-10-18）：打印 ac_registry::tasks::catalog() 的
    // 完整 JSON。它**不打开任何数据库、不写任何文件**，因此不会触发 rule2 红线；
    // 登记在这里是为了满足 every_example_is_registered_in_the_guard ——
    // examples/ 下新增文件必须与守门清单一致，否则它就游离在纪律之外。
    "tasks_probe.rs",
    // 任务路由表的证据导出器（2026-10-18）：只打印 Markdown、不打开库、不写文件。
    // 登记它不是因为它危险，而是守门是**文件名级**的：examples/ 下任何新文件
    // 都必须在这里出现，否则 every_example_is_registered_in_the_guard 会红。
    // 它抓到的正是「新加了 example 忘了登记」这一类漏网。
    "tasks_evidence.rs",
];

/// 唯一获准**直接** Registry::open 用户库路径的例程：它就是建库工具本身。
///
/// 授权是「文件名级」的，不是「这一行安全」——它仍然必须满足
/// guard_ingest_vendor_requires_explicit_absolute_path 那几条。
const AUTHORISED_WRITERS: &[&str] = &["ingest_vendor.rs"];

/// 剥掉注释，只留下真正会执行的代码（保留原始行号）。
///
/// 判定必须只看代码：文件头的警告注释里**必然**会出现 data/registry.db，
/// 若把注释算进来，这个测试对每个合规文件都会误报。
fn code_lines(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_block_doc = false;
    for (i, raw) in src.lines().enumerate() {
        let n = i + 1;
        let t = raw.trim_start();
        if in_block_doc {
            if t.contains("*/") {
                in_block_doc = false;
            }
            continue;
        }
        if t.starts_with("/*") {
            if !t.contains("*/") {
                in_block_doc = true;
            }
            continue;
        }
        if t.starts_with("//") {
            continue;
        }
        let code = t.split("//").next().unwrap_or("").trim();
        if code.is_empty() {
            continue;
        }
        out.push((n, code.to_string()));
    }
    out
}

/// 某个 example 的「代码」全文（已剥注释）。
fn code_of(name: &str) -> Vec<(usize, String)> {
    code_lines(&read_example(name))
}

fn joined(code: &[(usize, String)]) -> String {
    code.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>().join("\n")
}

// ─────────────────────────────────────────────────────────────────────
//  规则 2 的核心：路径来源的**数据流**判定（与命名无关）
// ─────────────────────────────────────────────────────────────────────

/// 一个路径表达式在数据流上的来源分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// 追不到任何「安全出处」—— 既不是副本，也不是临时目录，
    /// 也不是「这个文件根本没默认到用户库」情形下的命令行实参。
    Unknown,
    /// 值可证明来自 fs::copy 落点 / temp_dir / 本地新建 scratch 目录。
    DefinitiveSafe,
    /// 值来自命令行实参。只有当这个文件**没有**静默默认到用户库时才算安全；
    /// 规则 2 在文件层处理这个前提（见 rule2_red_line_violations）。
    CliArg,
}

impl Origin {
    fn is_definitive_safe(self) -> bool {
        matches!(self, Origin::DefinitiveSafe)
    }
    fn merge(self, other: Origin) -> Origin {
        if self.is_definitive_safe() {
            self
        } else {
            other
        }
    }
}

/// 判断一个路径表达式**拼完之后指向用户正式库**，即使它含有 temp_dir 之类的词。
///
/// 判据：路径里出现了指向 data/registry.db（或 registry.db）的那一段 join 尾巴。
/// 典型反向绕过：temp_dir().join("..").join("data").join("registry.db") ——
/// temp_dir 只是**参照点**，从那里往回 join 会落到磁盘上真实的用户库。
/// 只看「表达式里出现过 temp_dir」会把它放过，所以这里查终点。
fn terminal_is_user_db(expr: &str) -> bool {
    // 取最后一个 join 之后的片段，以及整串里是否同时出现 data + registry.db。
    let has_db_leaf = expr.contains("registry.db");
    let has_data_seg = expr.contains("data");
    let joined_back = expr.contains("join");
    has_db_leaf && has_data_seg && joined_back
}

/// 取出 Registry::open( 后面**第一个实参**的原文（到同层的第一个 ',' 或 ')'）。
///
/// 只取第一个实参是有意的：Registry::open 只有路径一个参数。
/// 若调用形态变了导致取不到，按 Unknown 处理 —— 保守判红。
fn registry_open_arg(line: &str) -> Option<&str> {
    let i = line.find("Registry::open(")?;
    let rest = &line[i + "Registry::open(".len()..];
    let mut depth = 0usize;
    for (off, ch) in rest.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    return Some(rest[..off].trim());
                }
                depth -= 1;
            }
            ',' if depth == 0 => return Some(rest[..off].trim()),
            _ => {}
        }
    }
    Some(rest.trim())
}

/// 把 std::path::Path::new(..) 这类**限定路径**整体折掉，只留最后一段，
/// 这样模块名 path 就不会和一个真名叫 path 的变量混同。
fn strip_qualified(expr: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let mut seg: String = chars[start..i].iter().collect();
            while i + 1 < chars.len() && chars[i] == ':' && chars[i + 1] == ':' {
                i += 2;
                if i < chars.len() && (chars[i].is_ascii_alphabetic() || chars[i] == '_') {
                    let s2 = i;
                    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    seg = chars[s2..i].iter().collect();
                }
            }
            out.push_str(&seg);
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 从实参里挑出**最像局部路径变量**的那个标识符。
///
/// 实参形态五花八门（&db、x.as_path()、Path::new(&s)、copied.clone()），
/// 决定「它是哪个变量」的只有一条：第一个既不是关键字、也不是已知
/// 类型/函数名的标识符。白名单里的名字来自语言与标准库层面，
/// 不是本仓库的约定，所以换个变量名不会绕过它。
fn candidate_binding(expr: &str) -> Option<String> {
    const KNOWN: &[&str] = &[
        "new", "from", "join", "as_path", "clone", "as_ref", "to_path_buf", "to_string_lossy",
        "as_str", "str", "Self", "env", "temp_dir", "current_dir", "current_exe", "args",
        "args_os", "nth", "unwrap_or_else", "unwrap_or", "unwrap_or_default", "expect", "Cow",
        "Rc", "Arc", "ref", "mut", "deref", "borrow", "replace", "with_capacity", "iter",
        "collect", "first", "map", "read", "copy", "write", "remove_dir_all", "create_dir_all",
        "display", "parent", "push", "pop", "len", "get",
    ];
    let folded = strip_qualified(expr);
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = folded.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            i += 1;
        }
    }
    for id in out {
        if KNOWN.contains(&id.as_str()) {
            continue;
        }
        if id.chars().next().is_some_and(|ch| ch.is_ascii_uppercase()) && id.len() <= 6 {
            continue;
        }
        return Some(id);
    }
    None
}

/// 收集源码里所有 fn <name>(..) { ... } 的函数体（用于追进辅助函数）。
fn collect_functions(code: &[(usize, String)]) -> BTreeMap<String, Vec<(usize, String)>> {
    let mut funcs: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    let mut cur_name: Option<String> = None;
    let mut cur_body: Vec<(usize, String)> = Vec::new();
    let mut depth = 0i32;
    let mut armed = false;
    for (ln, line) in code {
        let t = line.trim();
        if !armed {
            if let Some(rest) = t.strip_prefix("fn ") {
                let name = rest.split(['(', '<']).next().unwrap_or("").trim().to_string();
                if !name.is_empty() {
                    armed = true;
                    depth = 0;
                    cur_body.clear();
                    cur_name = Some(name);
                }
            } else {
                continue;
            }
        }
        cur_body.push((*ln, line.clone()));
        depth += t.matches('{').count() as i32;
        depth -= t.matches('}').count() as i32;
        if armed && depth <= 0 && t.ends_with('}') && cur_body.len() > 1 {
            if let Some(name) = cur_name.take() {
                funcs.insert(name, std::mem::take(&mut cur_body));
            }
            armed = false;
        }
    }
    if let Some(name) = cur_name {
        funcs.insert(name, cur_body);
    }
    funcs
}

/// 找到 let <id> = <rhs> 的定义，返回整条定义右侧的原文**（含续行）**。
///
/// # 为什么必须吃掉续行（这是 D1 之后补上的真实缺口）
///
/// 评审实证的反向绕过长这样：
///
///     let live = std::env::temp_dir()
///         .join("..")
///         .join("data")
///         .join("registry.db");
///     let reg = Registry::open(&live).expect("打开");
///
/// 只看**物理首行**的实现只会拿到 `std::env::temp_dir()` —— 后面的
/// .join("..").join("data").join("registry.db") 整个尾巴**从来没见过**。
/// 于是 terminal_is_user_db 判 false、origin_of_expr 判 DefinitiveSafe，
/// 一条从 temp 目录反向 join 回真实用户库的 open 被放行绿灯。
///
/// rustfmt 会把长链拆成多行，所以这不是刻意构造的刁难：只要路径超过一行，
/// 守门就静默失明。样本见 testdata/db_safety_guard/bypass_join_chain.rs。
fn binding_def<'a>(id: &str, code: &'a [(usize, String)]) -> Option<String> {
    let mut idx = 0usize;
    while idx < code.len() {
        let line = &code[idx].1;
        let mut rhs: Option<usize> = None;
        for kw in [format!("let {id} = "), format!("let mut {id} = ")] {
            if let Some(pos) = line.find(&kw) {
                rhs = Some(pos + kw.len());
                break;
            }
        }
        if rhs.is_none() {
            if let Some(pos) = line.find(&format!("let {id}: ")) {
                if let Some(eq) = line[pos..].find('=') {
                    rhs = Some(pos + eq + 1);
                }
            }
        }
        if let Some(start) = rhs {
            // 先收本行，再把被 rustfmt 拆走的链尾吃回来。
            let mut text = line[start..].trim().to_string();
            let mut k = idx + 1;
            while k < code.len() && needs_continuation(&text, &code[k].1) {
                text.push_str(code[k].1.trim());
                k += 1;
            }
            return Some(text);
        }
        idx += 1;
    }
    None
}

/// 这一行之后**还要继续吃下一行**吗？
///
/// 三个信号（少一个就会早停、漏掉链尾；多一个就会把后面的语句也吞进来）：
///   1. 括号没配平 —— 形如 foo(bar 的半截表达式；
///   2. 链尾没有收尾符 —— 没有 ';' / ')' / '}'，明显没写完；
///   3. **下一行以 '.' 开头** —— rustfmt 拆链的标准形态。
///
/// 第 3 条是关键：std::env::temp_dir() 本身括号是配平的、也以 ')' 收尾，
/// 单看本行像一条完整语句；但真正的判据在下一行的 .join("..") 上。
fn needs_continuation(cur: &str, next: &str) -> bool {
    let mut d = 0i32;
    for ch in cur.chars() {
        match ch {
            '(' | '[' | '{' => d += 1,
            ')' | ']' | '}' => d -= 1,
            _ => {}
        }
    }
    if d != 0 {
        return true;
    }
    let t = cur.trim_end();
    if !t.ends_with(';') && !t.ends_with(')') && !t.ends_with('}') {
        return true;
    }
    next.trim_start().starts_with('.')
}

/// 判断 id 是否是某个函数的形参（fn f(<id>: ..)）。
fn is_parameter_of(id: &str, code: &[(usize, String)]) -> bool {
    code.iter().any(|(_, l)| {
        let t = l.trim();
        if !t.starts_with("fn ") || !t.contains('(') {
            return false;
        }
        let Some(after) = t.split_once('(').map(|(_, a)| a) else {
            return false;
        };
        after
            .split(')')
            .next()
            .unwrap_or("")
            .split(',')
            .any(|p| {
                let p = p.trim().trim_start_matches("mut ").trim();
                p == id || p.starts_with(&format!("{id}:")) || p.starts_with(&format!("{id}: "))
            })
    })
}

/// 判断 id 是否源自**命令行实参**（args 集合的下标/nth/迭代/for）。
///
/// 覆盖：let args: Vec<String> = env::args().collect(); 后 args[1]/args.nth(1)；
/// for path in &args（migrate_check 就是这种）；env::args().nth(1)。
fn traces_to_cli_args(id: &str, code: &[(usize, String)]) -> bool {
    let mut saw_args_vec: Option<String> = None;
    for (_, line) in code {
        let t = line.trim();
        if t.contains("env::args()") {
            if let Some(rest) = t.strip_prefix("let ") {
                let name = rest
                    .split([':', ' ', '='])
                    .next()
                    .unwrap_or("")
                    .trim_start_matches("mut ")
                    .trim();
                if !name.is_empty() {
                    saw_args_vec = Some(name.to_string());
                }
            }
        }
        if let Some(rest) = t.strip_prefix("for ") {
            if let Some((pat, iter)) = rest.split_once(" in ") {
                let pat = pat.trim().trim_start_matches("mut ").trim();
                // 迭代器一侧可能带尾随的 '{'（for x in &args {），先切掉再比。
                let iter = iter.trim().trim_end_matches('{').trim();
                if (pat == id || pat.trim_start_matches('&') == id)
                    && saw_args_vec.as_deref() == Some(iter.trim_start_matches('&'))
                {
                    return true;
                }
            }
        }
    }
    if let Some(av) = &saw_args_vec {
        if let Some(def) = binding_def(id, code) {
            let def = def.as_str();
            if def.contains(av.as_str()) {
                return true;
            }
        }
    }
    false
}

/// 从 expr 里认出「调用了哪个自定义函数」。
fn callee_of(expr: &str) -> Option<String> {
    let e = expr.trim();
    let open = e.find('(')?;
    let raw = e[..open].trim();
    let name = raw.rsplit("::").next().unwrap_or(raw).trim();
    if name.is_empty() {
        return None;
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return None; // 类型名不是自定义函数
    }
    Some(name.to_string())
}

/// 这个文件里有没有「静默默认到用户库」的痕迹。
///
/// 有 = 该文件用同一个变量既可能装用户给的路径、也可能装用户库默认值，
/// 于是「这个 open 是用户显式选的」这句话**无法从源码证明**。
/// 无 = 这个 open 用的路径只可能来自 args，那就确实是用户的显式选择。
fn file_defaults_to_user_db(code: &[(usize, String)]) -> bool {
    code.iter().any(|(_, c)| {
        let literal = c.contains("data/registry.db")
            || (c.contains(".join(\"data\")") && c.contains("registry.db"))
            || (c.contains(".join(\"registry.db\")") && c.contains("data"))
            || (c.contains("\"data\"") && c.contains("\"registry.db\""));
        let as_default = c.contains("unwrap_or_else")
            || c.contains("unwrap_or(")
            || c.contains("unwrap_or_default(")
            || (c.contains("registry.db") && c.contains("to_string()"))
            || (c.contains("\"..\"") && c.contains("join") && c.contains("registry.db"));
        literal && as_default
    })
}

/// 判定一个表达式的来源（数据流，**不看名字**）。
fn origin_of_expr(
    expr: &str,
    code: &[(usize, String)],
    funcs: &BTreeMap<String, Vec<(usize, String)>>,
    depth: usize,
) -> Origin {
    if depth > 10 {
        return Origin::Unknown;
    }
    let e = expr.trim();

    // 直接形态：字面出现安全来源。
    // current_exe() 是 scratch 惯例的本地新建工作目录，不是用户正式库。
    if e.contains("fs::copy") || e.contains("temp_dir()") || e.contains("current_exe()") {
        return Origin::DefinitiveSafe;
    }
    if e.contains("args(") || e.contains("args_os(") {
        return Origin::CliArg;
    }
    // **反向绕过**：路径里出现 temp_dir 只是参照点；若后面还 join 回
    // data/registry.db，终点仍是用户正式库。见 terminal_is_user_db。
    if terminal_is_user_db(e) {
        return Origin::Unknown;
    }

    // 剥掉 .as_path() / .clone() / .to_path_buf() 等尾部方法。
    let core = e
        .split(".as_path()")
        .next()
        .unwrap_or(e)
        .split(".clone()")
        .next()
        .unwrap_or(e)
        .split(".to_path_buf()")
        .next()
        .unwrap_or(e);

    // 剥掉 & 与 Path::new(...) / PathBuf::from(...) 的最内层实参，得到核心标识符。
    let stripped = core.trim_start_matches('&');
    let mut inner = stripped.to_string();
    for ctor in ["Path::new(", "PathBuf::from("] {
        if let Some(idx) = inner.find(ctor) {
            let after = &inner[idx + ctor.len()..];
            // 取到第一个匹配 ')' 为止（我们的实参不含嵌套括号）。
            if let Some(end) = after.find(')') {
                inner = after[..end].trim().to_string();
            }
        }
    }

    if inner.contains("fs::copy") || inner.contains("temp_dir()") || inner.contains("current_exe()") {
        // temp_dir 只是参照点：若终点仍拼回 data/registry.db，则不是副本。
        if !terminal_is_user_db(&inner) {
            return Origin::DefinitiveSafe;
        }
        return Origin::Unknown;
    }
    if inner.contains("args(") || inner.contains("args_os(") {
        return Origin::CliArg;
    }

    let Some(id) = candidate_binding(&inner) else {
        return Origin::Unknown;
    };

    // 形态：let id = <def> —— 追进去。
    if let Some(def) = binding_def(&id, code) {
        let def = def.as_str();
        if def.contains("fs::copy") || def.contains("temp_dir()") || def.contains("current_exe()") {
            if !terminal_is_user_db(def) {
                return Origin::DefinitiveSafe;
            }
            return Origin::Unknown;
        }
        if def.contains("args(") || def.contains("args_os(") || traces_to_cli_args(&id, code) {
            return Origin::CliArg;
        }
        // def 调了自定义函数 → 追进那个函数体。
        if let Some(fname) = callee_of(def) {
            if let Some(body) = funcs.get(&fname) {
                let o = function_return_origin(body, funcs, depth + 1);
                if o != Origin::Unknown {
                    return o;
                }
            }
        }
        // def 是另一个绑定 → 递归。
        if let Some(next) = candidate_binding(def) {
            if next != id {
                return origin_of_expr(def, code, funcs, depth + 1);
            }
        }
        return Origin::Unknown;
    }

    // 形态：id 源自命令行实参。
    if traces_to_cli_args(&id, code) {
        return Origin::CliArg;
    }
    // 形态：id 是某个函数的形参 → 调用点会各自判定。
    if is_parameter_of(&id, code) {
        return Origin::Unknown; // 保守：形参本身不能证明安全
    }

    Origin::Unknown
}

/// 计算一个自定义函数的**返回值**来源。
fn function_return_origin(
    body: &[(usize, String)],
    funcs: &BTreeMap<String, Vec<(usize, String)>>,
    depth: usize,
) -> Origin {
    if depth > 10 {
        return Origin::Unknown;
    }
    let mut acc = Origin::Unknown;
    for (_, line) in body {
        let t = line.trim();
        if let Some(r) = t.strip_prefix("return ") {
            acc = acc.merge(origin_of_expr(r, body, funcs, depth + 1));
        }
    }
    // 兜底：函数体里出现了 scratch/copy/temp_dir 的**构造**（不是 args！），
    // 说明返回值是一个本地新建目录下的路径。只有 DefinitiveSafe 类，
    // args 类在这里**不**兜底 —— 那正是事故形态（db_path 里既有 args 又有默认值）。
    if body.iter().any(|(_, l)| {
        l.contains("fs::copy") || l.contains("temp_dir()") || l.contains("current_exe()")
    }) {
        acc = acc.merge(Origin::DefinitiveSafe);
    }
    acc
}

/// 规则 2 的判定：**对一段代码里的每个 Registry::open，实参必须可证明不是用户库**。
///
/// 返回违规列表（(行号, 实参原文, 理由)）。空列表 = 通过。
///
/// 判定与命名无关，不是「这个变量名危险吗」，
/// 而是「这个路径能不能证明不是用户正式库」。
fn rule2_red_line_violations(
    example_name: &str,
    code: &[(usize, String)],
) -> Vec<(usize, String, String)> {
    let mut violations = Vec::new();
    if AUTHORISED_WRITERS.contains(&example_name) {
        // 建库工具，由 guard_ingest_vendor_requires_explicit_absolute_path 单独盯。
        return violations;
    }
    let funcs = collect_functions(code);
    let defaults_to_user_db = file_defaults_to_user_db(code);
    for (ln, line) in code {
        if !line.contains("Registry::open(") {
            continue;
        }
        let arg = registry_open_arg(line).unwrap_or("").to_string();
        let origin = origin_of_expr(&arg, code, &funcs, 0);
        // 唯一的放行条件：DefinitiveSafe（副本/临时目录/scratch）。
        // CliArg 只有在「这个文件根本没默认到用户库」时才安全；否则判红。
        let safe = origin.is_definitive_safe()
            || (origin == Origin::CliArg && !defaults_to_user_db);
        if !safe {
            let why = if origin == Origin::CliArg {
                format!(
                    "实参「{}」虽然来自命令行实参，但**这个文件本身会把 data/registry.db \
                     作为默认路径**（事故形态：同一个变量既装用户给的路径又装用户库默认值，\
                     「用户显式选的」这件事从源码无法证明）。",
                    arg
                )
            } else {
                format!(
                    "实参「{}」无法证明来自 fs::copy 的落点 / temp_dir / 本地新建 scratch 目录。",
                    arg
                )
            };
            violations.push((*ln, line.clone(), why));
        }
    }
    violations
}

/// 判定「提到用户库」的例程给出了安全出路（copy-first / 只读 / 只打印计划）。
fn has_safe_exit(code: &[(usize, String)]) -> bool {
    let all = joined(code);
    let copy_first = all.contains("fs::copy") && all.contains("fs::read");
    let read_only = all.contains("mode=ro");
    let plan_only = all.contains("let Some(db) = db else");
    copy_first || read_only || plan_only
}

/// 判断代码里是否出现了「兜底到用户库」的字面量 / join 拼接形态。
fn has_default_real_db(code: &[(usize, String)]) -> bool {
    file_defaults_to_user_db(code)
}

// ─────────────────────────────────────────────────────────────────────
//  守门测试
// ─────────────────────────────────────────────────────────────────────

#[test]
fn guard_no_example_opens_real_registry_db_read_write() {
    for name in WATCHED {
        let code = code_of(name);

        // ── 1) 兜底路径必须配一条安全出路 ──────────────────────────
        let has_default = has_default_real_db(&code);
        assert!(
            !has_default || has_safe_exit(&code),
            "{name}: 把 data/registry.db 作为默认/兜底路径，却没有配套的安全机制。\n\n\
             无参运行必须满足三者之一：\n\
               (a) copy-first：std::fs::copy 到工作目录 + std::fs::read 取 SHA256，\n\
                   并且从对该路径的副本调用 Registry::open；\n\
               (b) 只读：以 SQLite URI mode=ro 打开（不做迁移、不建表）；\n\
               (c) 无参即退出：只打印将要做什么，不做任何写入。\n\n\
             历史事故：直接 open 用户库会重写 SQLite 页（见本文件头）。"
        );

        // ── 2) 红线：Registry::open 的每个实参都必须可证明不是用户库 ──
        let violations = rule2_red_line_violations(name, &code);
        assert!(
            violations.is_empty(),
            "{name}: Registry::open 红线被绕过（与命名无关的判定）:\n{}",
            violations
                .iter()
                .map(|(ln, line, why)| format!("  第{ln}行: {line}\n  -> {why}\n"))
                .collect::<Vec<_>>()
                .join("\n")
        );

        // ── 3) 走 copy-first 的例程必须同时有取证手段 ─────────────
        let all = joined(&code);
        if all.contains("fs::copy") {
            assert!(
                all.contains("fs::read"),
                "{name}: 已经用了 std::fs::copy 说明走的是 copy-first 路线，\n\
                 但缺少 std::fs::read 取证。证明原库没被动过只能靠 std::fs::read\n\
                 读原始字节；再开一次库去比对本身就是会写库的。"
            );
        }
    }
}

#[test]
fn guard_ingest_vendor_requires_explicit_absolute_path() {
    let all = joined(&code_of("ingest_vendor.rs"));

    assert!(
        all.contains("is_absolute()"),
        "ingest_vendor: 写库必须要求**绝对路径** db 参数；相对路径正是那个静默默认的形态。"
    );
    assert!(
        all.contains("let Some(db) = db else"),
        "ingest_vendor: 无参运行必须走 else 分支（只打印计划并退出），不得静默 fallback 到默认库。"
    );
    assert!(
        !all.contains("unwrap_or_else(|| root_dir.join(\"data\")"),
        "ingest_vendor: 不得再把 <包根>/data/registry.db 作为**兜底**路径（只允许出现在只打印计划的分支里）。"
    );
    assert!(
        all.contains("default_db"),
        "ingest_vendor: 无参分支应当只打印 default_db 这个计划，不该写它。"
    );
}

/// D4：ingest_vendor 除了写库，还会覆盖两个 preview.json。
///
/// 它们不是数据库，所以上面那条红线（Registry::open）根本看不到它们——
/// 这恰好是它们必须另行声明的原因。这条测试把「声明了」变成
/// 可被机械验证的事实：以后新增第三个覆盖路径，它必须同样报出来。
#[test]
fn guard_ingest_vendor_announces_every_path_it_overwrites() {
    let src = read_example("ingest_vendor.rs");
    let code = code_lines(&src);

    // 本次运行会写的文件（以 fs::write / fs::copy 的目标表达结果为准）。
    let written: Vec<&str> = ["preview.json", "registry.db"].into_iter().collect();
    for w in written {
        assert!(
            src.contains(w),
            "ingest_vendor 里找不到 {w} 的写入路径；本测试需要同步更新。"
        );
    }

    // 每个被覆盖的目标，都必须在主流程的输出里被明写。
    let announced = count_announcements(&code, "preview.json");
    assert!(
        announced >= 2,
        "ingest_vendor 覆盖了 preview.json（public/ 与 dist/ 各一份），\n\
         但主流程里只声明了 {announced} 处。\n\
         每一个静默被覆盖的文件都必须在写之前被明写；\n\
         见 main() 里的「会被覆盖」列表。"
    );
    // 不得回退成静默：这两次写入不得在无参分支里发生。
    assert!(
        src.contains("let Some(db) = db else"),
        "无参分支必须仍然只打印计划并退出，不得写任何文件。"
    );
}

/// 数一数代码里出现了多少次「声明这个文件会被覆盖」。
fn count_announcements(code: &[(usize, String)], target: &str) -> usize {
    code.iter()
        .filter(|(_, l)| l.contains(target) && l.contains("会被覆盖"))
        .count()
}

#[test]
fn guard_verify_db_stays_read_only() {
    let all = joined(&code_of("m2_verify_db.rs"));
    assert!(
        all.contains("mode=ro"),
        "m2_verify_db: 必须继续以 SQLite URI mode=ro 打开。它不做迁移、不建表。"
    );
    assert!(
        !all.contains("Registry::open"),
        "m2_verify_db: 不得改用 Registry::open —— 那会执行迁移，是写操作。"
    );
}

#[test]
fn every_example_is_registered_in_the_guard() {
    let on_disk = example_names();
    let mut watched: Vec<String> = WATCHED.iter().map(|s| s.to_string()).collect();
    watched.sort();
    assert_eq!(
        on_disk, watched,
        "examples/ 下的文件与守门清单不一致。\n\n\
         新增 example 必须登记进 WATCHED，否则它不受这条纪律约束。"
    );
}

/// 守门本身必须是活的：事故版本的原始写法必须被判失败。
#[test]
fn the_guard_actually_rejects_the_original_incident() {
    let incident = [
        "fn db_path() -> String {",
        "    let args: Vec<String> = std::env::args().collect();",
        "    if args.len() > 1 {",
        "        return args[1].clone();",
        "    }",
        "    \"../data/registry.db\".to_string()",
        "}",
        "fn main() {",
        "    let path = db_path();",
        "    let reg = Registry::open(std::path::Path::new(&path)).expect(\"open failed\");",
        "    let _ = reg;",
        "}",
    ]
    .join("\n");

    let code = code_lines(&incident);
    assert!(has_default_real_db(&code), "事故样本本身应当构成兜底路径");
    assert!(
        !has_safe_exit(&code),
        "事故样本必须被判为不安全（否则守门规则失效）"
    );

    let violations = rule2_red_line_violations("incident", &code);
    assert!(
        !violations.is_empty(),
        "事故样本必须命中 Registry::open 红线（否则守门规则失效）；实际 violations={violations:?}"
    );
}

/// **关键自检**：守门自带的绕过样本必须被判红，且**调用的是真实规则函数**。
///
/// 上一版红线是一张字面变量名清单，bypass_live_var.rs（把 path 改名成 live）
/// 与事故版本行为完全一致，却能静默通过。这里把样本文件钉死在仓库里，
/// 并要求守门必须判它红 —— 一次改名就能让缺陷变绿灯的守门，比没有守门更危险。
///
/// （本测试**直接调用 rule2_red_line_violations**，与主守门测试同一条代码路径；
///   绝不复制一份规则逻辑，否则就变成第二条会漂移的规则。）
#[test]
fn bypass_fixtures_are_rejected_by_rule_2() {
    let cases = [
        ("bypass_live_var.rs", "变量改名绕过（评审实证）"),
        ("bypass_helper_fn.rs", "辅助函数 + PathBuf::from().join() 拼接绕过"),
        ("bypass_join_chain.rs", "内联 join 链 / 把 temp_dir 当免罪符"),
    ];
    for (file, why) in cases {
        let p = guard_testdata_dir().join(file);
        let src = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("读不到绕过样本 {}: {e}", p.display()));
        let code = code_lines(&src);
        let violations = rule2_red_line_violations(file, &code);
        assert!(
            !violations.is_empty(),
            "绕过样本 {file}（{why}）竟然通过了规则 2 —— 守门对它无效。\n\
             这正是「只认某几个变量名 = 假的安全感」的实证。样本路径: {}",
            p.display()
        );
    }
}

/// 反向自检：守门自带的**合规**样本必须通过（防止规则写成恒红 / 误伤正确写法）。
#[test]
fn safe_fixtures_pass_rule_2() {
    for name in ["m2_e2e.rs", "m2_fulltext_e2e.rs", "migrate_check.rs"] {
        let code = code_of(name);
        let violations = rule2_red_line_violations(name, &code);
        assert!(
            violations.is_empty(),
            "合规例程 {name} 被规则 2 误判为红线:\n{violations:?}"
        );
    }
}
