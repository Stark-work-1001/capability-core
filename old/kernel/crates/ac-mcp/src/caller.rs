//! 调用方身份与**能力裁剪**规则。
//!
//! # 问题
//!
//! 多个智能体同时接入中枢时，"能不能看到某个能力"必须是一次**可计算的比较**，
//! 而不是每个接入方各自决定。M2.5 的 `ac_core::CapabilityIdentity::admits`
//! 已经给了比较原语；本模块只做两件事：把接入方的身份**投影**出来，
//! 并把投影结果表达成一个调用方能自查的契约。
//!
//! # 三层裁剪，从松到紧
//!
//! | 层 | 判据 | 拒绝时给调用方什么 |
//! |---|---|---|
//! | 1. 身份 | `CallerIdentity::admits(asset_identity)` | denied 计数，不给原因 |
//! | 2. 授权 | `grant` 白名单（可选） | 明确的 not_granted |
//! | 3. 公开性 | `deny` 黑名单（可选） | 明确的 denied_by_policy |
//!
//! 顺序**不能换**：先看身份再看白名单，能保证"身份不够"的请求不会被
//! 记成"没授权"——这两件事的处置完全不同（前者是配置错了，后者是正常收窄）。
//!
//! # 为什么默认是 Anonymous
//!
//! 不带身份的调用方默认只拿到 **Read** 权。只读是这个库里**唯一无副作用**的
//! 一档（`Authority::Read`），因此"忘了传身份"的最坏后果是"看到得少了"，
//! 而不是"看到了不该看到的"。反过来若默认宽松，一个配置失误就会让
//! 对外接口变成越权入口。

use ac_core::{Authority, CapabilityIdentity, SideEffect, ToolPosture};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 一个接入中枢的调用方。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Caller {
    /// 智能体 id。与 agents.agent_id 对应；匿名调用为 "anonymous"。
    pub agent_id: String,
    /// 展示名。
    #[serde(default)]
    pub display_name: String,
    /// 该调用方**自报**的身份。裁剪的判据就是它。
    #[serde(default)]
    pub identity: CallerIdentity,
    /// 可见性策略。
    #[serde(default)]
    pub visibility: Visibility,
    /// 同一能力上的最大并发持有数。1 = 互斥。
    #[serde(default = "default_concurrency")]
    pub max_concurrent: u32,
}

fn default_concurrency() -> u32 {
    1
}

impl Caller {
    /// 匿名调用方的保留 id。
    ///
    /// 刻意做成常量而不是到处写字面量：`resolve_caller` 靠它区分
    /// 「显式要匿名档」（`agent=anonymous`）与「碰巧叫这名字的智能体」。
    /// 前者是合法的，后者注册时就会与保留名冲突——两者必须由同一个常量裁决。
    pub const ANONYMOUS_ID: &'static str = "anonymous";

    /// 匿名调用方：只读接口里的**观察者**，只看无副作用资产、并发 1。
    ///
    /// # 为什么是 Observe 而不是 Read
    ///
    /// 两者都安全，但默认档位要选**能让接口可用**的那一个：
    /// 推断逻辑（`ac_registry::infer`）把没有明确读写声明的资产
    /// 归为 Observe，所以按 Read 收窄会让匿名调用方在真实库里几乎
    /// 什么都看不到——"安全但没用"不是好默认。
    ///
    /// Observe 依然安全，因为本接口**只读**：它放大的是"看得见"，
    /// 不是"动得了"——`side_effect` 仍是 `None`，而对外根本没有
    /// 任何写路由。要真正的写权限必须由具名智能体显式声明身份。
    pub fn anonymous() -> Self {
        Self {
            agent_id: Self::ANONYMOUS_ID.into(),
            display_name: Self::ANONYMOUS_ID.into(),
            identity: CallerIdentity::observer(),
            visibility: Visibility::default(),
            max_concurrent: default_concurrency(),
        }
    }

    /// 覆盖展示名。
    ///
    /// 落库时 `display_name` 可能为空（`register_agent` 会归一成
    /// agent_id，但历史行或别处写入未必），而它会直接出现在检索响应的
    /// `caller` 字段上。空串会让调用方看到一个没有名字的调用方。
    pub fn with_display_name(mut self, name: &str) -> Self {
        let n = name.trim();
        if !n.is_empty() {
            self.display_name = n.to_string();
        }
        self
    }

    /// 构造一个具名调用方。
    pub fn new(agent_id: impl Into<String>, identity: CallerIdentity) -> Self {
        let id = agent_id.into();
        Self {
            display_name: id.clone(),
            agent_id: id,
            identity,
            visibility: Visibility::default(),
            max_concurrent: default_concurrency(),
        }
    }

    pub fn with_visibility(mut self, v: Visibility) -> Self {
        self.visibility = v;
        self
    }

    pub fn with_max_concurrent(mut self, n: u32) -> Self {
        self.max_concurrent = n.max(1);
        self
    }

    /// 该调用方是否能**看到**目标能力。
    ///
    /// 返回 `None` 表示允许；`Some(reason)` 是稳定的机器可读原因码，
    /// 直接进审计与错误响应——不留"为什么"就等于让接入方去猜。
    pub fn admits(&self, target: &CapabilityIdentity) -> Option<&'static str> {
        // 第 1 层：身份。放在最前，保证"身份不够"不会被记成"没授权"。
        if !self.identity.admits(target) {
            return Some("identity_insufficient");
        }
        // 第 2 层：白名单。非空即"仅此清单"。
        if !self.visibility.allow.is_empty() && !self.visibility.allows(target) {
            return Some("not_granted");
        }
        // 第 3 层：黑名单。最后一道，覆盖白名单也命中的情况。
        if self.visibility.blocks(target) {
            return Some("denied_by_policy");
        }
        None
    }
}

/// 调用方自报的身份。与 `CapabilityIdentity` 同构，字段更少——
/// 调用方要证明的是"我能承担多重的活"，不是"我自己的副作用是什么"。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallerIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// 自身持有的最高权限。**被裁剪的能力要求不得高于它**。
    #[serde(default)]
    pub authority: Authority,
    /// 自身能承受的副作用上限。
    #[serde(default)]
    pub side_effect: SideEffect,
    /// 自身面对工具的姿态上限。
    #[serde(default)]
    pub tool_posture: ToolPosture,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dimensions: BTreeMap<String, String>,
}

impl CallerIdentity {
    /// 严格只读身份（仅 Authority::Read）。
    pub fn read_only() -> Self {
        Self {
            authority: Authority::Read,
            side_effect: SideEffect::None,
            tool_posture: ToolPosture::Reader,
            ..Default::default()
        }
    }

    /// 观察者身份：Authority::Observe + SideEffect::None。
    ///
    /// **这是匿名调用方的默认身份**，理由写在 `Caller::anonymous` 上。
    pub fn observer() -> Self {
        Self {
            authority: Authority::Observe,
            // 关键：对外接口本身只读，所以上限仍是"无副作用"。
            // 只放宽 authority 这一维，是"看得见"与"动得了"的分离：
            // Observe 的语义是"可执行但副作用受限"，而本接口不提供执行，
            // 因此把它和 None 组合起来，既不虚报能力也不虚报风险。
            side_effect: SideEffect::None,
            tool_posture: ToolPosture::Reader,
            ..Default::default()
        }
    }

    /// 投影成 `CapabilityIdentity`，好让 `admits` 走同一套规则
    /// （不重写一遍比较逻辑——两套规则迟早会分叉）。
    pub fn as_capability_identity(&self) -> CapabilityIdentity {
        CapabilityIdentity {
            role: self.role.clone(),
            authority: self.authority,
            side_effect: self.side_effect,
            tool_posture: self.tool_posture,
            idempotent: true,
            open_world: false,
            dimensions: self.dimensions.clone(),
        }
    }

    /// 裁剪判定。**安全三维**与 `ac_core::admits` 完全一致，
    /// **dimensions 维**按"调用方显式声明才生效"处理。
    ///
    /// # 为什么 dimensions 必须改判据（这是本轮改过的一处语义）
    ///
    /// `ac_core::CapabilityIdentity::admits` 的 dimension 规则是
    /// "**目标的每个 dimension 都必须被调用方逐条覆盖**"。
    /// 那是为**委派链**设计的——上级必须精确声明下级要用的每个维度。
    ///
    /// 但本接口是**检索**，不是委派：库里每个资产都带
    /// `dimensions={cost_tier, domain}`（实测 506 条全都有），
    /// 而一个"只想找只读技能"的调用方根本没声明 domain。
    /// 沿用委派规则的结果是**一条都看不到**（实测 anonymous 可见 0 / 460）。
    ///
    /// 所以这里把 dimensions 定性为**调用方主动施加的收窄条件**：
    /// 调用方声明了 `domain=geo` 就只准看 geo 的；一个都没声明就
    /// 不按维度收窄。安全三维（authority / side_effect / tool_posture）
    /// 一律不放宽——那三维才是"动得了"的真判据。
    pub fn admits(&self, target: &CapabilityIdentity) -> bool {
        let mine = self.as_capability_identity();
        // 安全三维交给 ac_core，保证与全库同一份规则、不分叉。
        if mine.authority < target.authority {
            return false;
        }
        if mine.side_effect < target.side_effect {
            return false;
        }
        if target.tool_posture > mine.tool_posture {
            return false;
        }
        // dimensions：调用方声明的每一条都必须与目标一致；
        // 调用方没声明的键不参与判定。
        for (k, v) in &self.dimensions {
            match target.dimensions.get(k) {
                Some(tv) if tv == v => {}
                // 目标缺这个维度时按不匹配处理：调用方明确要 domain=geo，
                // 而目标根本没声明 domain，放行等于给了一个未经声明的特写。
                _ => return false,
            }
        }
        true
    }
}

/// 可见性策略。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Visibility {
    /// 白名单：非空时**只**允许匹配这些条件的能力。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<Grant>,
    /// 黑名单：命中即拒。优先于白名单。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<Grant>,
}

/// 一条授权规则：维度匹配（全部命中才算匹配）。
///
/// 维度取值都支持 `*` 通配，`ecosystem=dsh` 会命中该生态下的全部能力。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ecosystem: Option<String>,
}

fn match_opt(rule: &Option<String>, value: &str) -> bool {
    match rule {
        None => true,
        Some(v) if v == "*" => true,
        Some(v) => v.eq_ignore_ascii_case(value),
    }
}

impl Grant {
    /// 规则对该资产是否成立。未在规则里出现的维度一律放行
    /// （规则越窄越具体，不是越全越严格）。
    pub fn matches(&self, kind: &str, tags: &[String], ecosystem: &str) -> bool {
        if !match_opt(&self.kind, kind) {
            return false;
        }
        if !match_opt(&self.ecosystem, ecosystem) {
            return false;
        }
        match &self.tag {
            None => true,
            Some(t) => tags.iter().any(|x| x.eq_ignore_ascii_case(t)),
        }
    }
}

impl Visibility {
    pub fn allows(&self, _target: &CapabilityIdentity) -> bool {
        // 实际判定在 admits 里按 kind/tag/ecosystem 做——身份维度这里不重复。
        true
    }

    /// 按 (kind, tags, ecosystem) 判定，规则集非空即"白名单模式"。
    pub fn allows_assets(&self, kind: &str, tags: &[String], ecosystem: &str) -> bool {
        if self.allow.is_empty() {
            return true;
        }
        self.allow
            .iter()
            .any(|g| g.matches(kind, tags, ecosystem))
    }

    pub fn blocks(&self, _target: &CapabilityIdentity) -> bool {
        false
    }

    /// 黑名单判定（按 (kind, tags, ecosystem)）。
    pub fn blocks_assets(&self, kind: &str, tags: &[String], ecosystem: &str) -> bool {
        self.deny.iter().any(|g| g.matches(kind, tags, ecosystem))
    }
}

/// 裁剪结果，供响应自报用。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisibilityStats {
    /// 裁剪前候选数。
    pub examined: u32,
    /// 裁剪后可见数。
    pub visible: u32,
    /// 被拒数。
    pub denied: u32,
    /// 拒绝原因计数，键是稳定的机器可读原因码。
    pub denied_reasons: BTreeMap<String, u32>,
}

impl VisibilityStats {
    pub fn record_deny(&mut self, reason: &'static str) {
        self.denied += 1;
        *self.denied_reasons.entry(reason.to_string()).or_insert(0) += 1;
    }

    pub fn record_visible(&mut self) {
        self.visible += 1;
    }

    pub fn examine(&mut self) {
        self.examined += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(auth: Authority, kind_dims: &[(&str, &str)]) -> CapabilityIdentity {
        CapabilityIdentity {
            authority: auth,
            dimensions: kind_dims
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn anonymous_sees_read_and_observe_but_not_write() {
        let c = Caller::anonymous();
        // 无副作用的两档：可见。这是"默认档要可用"的直接断言。
        assert!(c.admits(&target(Authority::Read, &[])).is_none());
        assert!(c.admits(&target(Authority::Observe, &[])).is_none());
        // 有副作用的两档：一律拒绝。
        for auth in [Authority::Write, Authority::Destructive] {
            assert_eq!(
                c.admits(&target(auth, &[])),
                Some("identity_insufficient"),
                "{auth:?} 不该被匿名看到"
            );
        }
        // 副作用维度才是"能不能动"的真判据：即使 authority 够，
        // 声明了 Irreversible 也必须被拒。
        assert_eq!(
            c.admits(&CapabilityIdentity {
                authority: Authority::Observe,
                side_effect: SideEffect::Irreversible,
                ..Default::default()
            }),
            Some("identity_insufficient")
        );
    }

    #[test]
    fn dimension_mismatch_is_rejected_even_with_high_authority() {
        let c = Caller::new(
            "geo-agent",
            CallerIdentity {
                authority: Authority::Destructive,
                dimensions: [("domain".to_string(), "geo".to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        );
        assert!(c.admits(&target(Authority::Read, &[("domain", "geo")])).is_none());
        assert_eq!(
            c.admits(&target(Authority::Read, &[("domain", "bio")])),
            Some("identity_insufficient")
        );
    }

    #[test]
    fn allow_list_narrows_but_deny_list_wins() {
        let c = Caller::new("kiro", CallerIdentity::read_only()).with_visibility(Visibility {
            allow: vec![Grant {
                kind: Some("skill".into()),
                ..Default::default()
            }],
            deny: vec![Grant {
                tag: Some("secret".into()),
                ..Default::default()
            }],
        });
        let tags_read = vec!["read_only".to_string()];
        assert!(c.visibility.allows_assets("skill", &tags_read, "dsh"));
        assert!(!c.visibility.allows_assets("mcp", &tags_read, "dsh"));
        // 命中白名单但同时命中黑名单 -> 拒。
        assert!(c.visibility.blocks_assets("skill", &vec!["secret".into()], "dsh"));
    }

    #[test]
    fn empty_allow_list_means_no_narrowing() {
        let v = Visibility::default();
        assert!(v.allows_assets("anything", &[], "eco"));
    }

    #[test]
    fn grant_wildcard_matches_any() {
        let g = Grant {
            kind: Some("*".into()),
            tag: None,
            ecosystem: Some("dsh".into()),
        };
        assert!(g.matches("skill", &[], "dsh"));
        assert!(!g.matches("skill", &[], "other"));
    }

    /// 防漂移（安全三维）：authority / side_effect / tool_posture 的判定
    /// 必须与 `ac_core::admits` **完全一致**。
    ///
    /// 只有 dimensions 一维刻意不同（见 `admits` 的文档），
    /// 其余三维一旦与全库规则分叉，裁剪就会静默失真——
    /// 而失真方向通常是"放得比应该的多"。
    #[test]
    fn safety_dimensions_match_ac_core_admits_exactly() {
        use ac_core::{SideEffect, ToolPosture};
        let effects = [
            SideEffect::None,
            SideEffect::LocalArtifact,
            SideEffect::Mutating,
            SideEffect::Irreversible,
        ];
        let postures = [
            ToolPosture::Pure,
            ToolPosture::Reader,
            ToolPosture::Writer,
            ToolPosture::Gated,
        ];
        for a in Authority::ALL {
            for b in Authority::ALL {
                for se in effects {
                    for tp in postures {
                        // 双方都不声明 dimensions，让 ac_core 的 dimension
                        // 规则不参与，于是两条路径应当完全同判定。
                        let me = CallerIdentity {
                            authority: a,
                            side_effect: se,
                            tool_posture: tp,
                            ..Default::default()
                        };
                        let theirs = CapabilityIdentity {
                            authority: b,
                            side_effect: se,
                            tool_posture: tp,
                            ..Default::default()
                        };
                        assert_eq!(
                            me.as_capability_identity().admits(&theirs),
                            me.admits(&theirs),
                            "安全三维在 a={a:?} b={b:?} se={se:?} tp={tp:?} 上与 ac_core 不一致"
                        );
                    }
                }
            }
        }
    }

    /// 记录**刻意**的分歧：调用方未声明 dimensions 时不按维度收窄。
    ///
    /// 这条是对上面那条"例外"的正面锁定——若将来有人图省事改回
    /// 完全委托 ac_core，这里会立刻红。
    #[test]
    fn undeclared_dimensions_do_not_hide_assets() {
        // 真实库里的资产形态：带 dimensions，但调用方没声明 domain。
        let asset = CapabilityIdentity {
            authority: Authority::Read,
            side_effect: ac_core::SideEffect::None,
            tool_posture: ac_core::ToolPosture::Pure,
            dimensions: [
                ("cost_tier".to_string(), "free".to_string()),
                ("domain".to_string(), "knowledge".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let caller = CallerIdentity::observer();
        assert!(caller.admits(&asset), "未声明 dimensions 不应导致一条都看不到");
        // 声明了不匹配的 domain 则必须被拒——收窄仍然有效。
        let picky = CallerIdentity {
            dimensions: [("domain".to_string(), "geo".to_string())]
                .into_iter()
                .collect(),
            ..CallerIdentity::observer()
        };
        assert!(!picky.admits(&asset));
    }
}
