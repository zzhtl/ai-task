//! 强类型 ID。
//!
//! 全部用 UUIDv7 而不是自增 BIGINT：ID 出现在 URL 里、会被远端 agent 和
//! `claude --session-id` 携带，不能可枚举；UUIDv7 是时间有序的，不会像 v4
//! 那样打散 B-tree。对客户端它们是不透明字符串。

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[ts(export, type = "string")]
        pub struct $name(pub Uuid);

        impl $name {
            /// 生成一个新的时间有序 ID。
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.parse()?))
            }
        }
    };
}

typed_id!(/// 工作区（多租户边界，v1 只有一个）。
    WorkspaceId);
typed_id!(/// 用户。
    UserId);
typed_id!(/// 任务定义。
    TaskId);
typed_id!(/// 任务定义的不可变版本快照。
    TaskVersionId);
typed_id!(/// 定时触发配置。
    ScheduleId);
typed_id!(/// 一次执行。
    RunId);
typed_id!(/// 目标主机。
    HostId);
typed_id!(/// 加密存储的凭据。
    CredentialId);
typed_id!(/// 技能包。
    SkillId);
typed_id!(/// 规则（软规则或硬策略）。
    RuleId);
typed_id!(/// 人工审批请求。
    ApprovalId);

/// DAG 内的节点标识，由用户在编排时指定，在一个任务版本内唯一。
///
/// 不用 UUID：节点 key 会出现在 `inputs` 的 JSONPath 引用里，要人可读、可手写。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NodeKey(pub String);

impl NodeKey {
    /// 节点 key 的最大长度。会进 JSONPath 表达式和事件负载，必须有界。
    pub const MAX_LEN: usize = 64;

    /// 校验节点 key：非空、不超长、只允许 `[A-Za-z0-9_-]`。
    ///
    /// 收紧字符集是为了让 key 能安全地直接拼进 JSONPath 与文件路径，
    /// 不需要在每个使用点再做一次转义。
    pub fn parse(s: impl Into<String>) -> Result<Self, InvalidNodeKey> {
        let s = s.into();
        if s.is_empty() {
            return Err(InvalidNodeKey::Empty);
        }
        if s.len() > Self::MAX_LEN {
            return Err(InvalidNodeKey::TooLong(s.len()));
        }
        if let Some(c) = s
            .chars()
            .find(|c| !matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '-'))
        {
            return Err(InvalidNodeKey::BadChar(c));
        }
        Ok(Self(s))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// [`NodeKey::parse`] 的拒绝原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidNodeKey {
    Empty,
    TooLong(usize),
    BadChar(char),
}

impl std::fmt::Display for InvalidNodeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("节点 key 不能为空"),
            Self::TooLong(n) => write!(f, "节点 key 长度 {n} 超过上限 {}", NodeKey::MAX_LEN),
            Self::BadChar(c) => write!(f, "节点 key 含非法字符 {c:?}，只允许 A-Z a-z 0-9 _ -"),
        }
    }
}

impl std::error::Error for InvalidNodeKey {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_ids_are_time_ordered() {
        let a = RunId::new();
        let b = RunId::new();
        assert!(a < b, "UUIDv7 必须单调递增，否则索引局部性失效");
    }

    #[test]
    fn typed_ids_serialize_as_bare_strings() {
        let id = RunId::new();
        let json = serde_json::to_string(&id).expect("序列化");
        assert_eq!(json, format!("\"{id}\""));
    }

    #[test]
    fn node_key_accepts_plain_identifiers() {
        assert!(NodeKey::parse("collect-hosts").is_ok());
        assert!(NodeKey::parse("summarize_1").is_ok());
    }

    #[test]
    fn node_key_rejects_path_and_jsonpath_metacharacters() {
        assert_eq!(NodeKey::parse(""), Err(InvalidNodeKey::Empty));
        assert_eq!(NodeKey::parse("a/b"), Err(InvalidNodeKey::BadChar('/')));
        assert_eq!(NodeKey::parse("a.b"), Err(InvalidNodeKey::BadChar('.')));
        assert_eq!(NodeKey::parse("../etc"), Err(InvalidNodeKey::BadChar('.')));
        assert_eq!(NodeKey::parse("a['b']"), Err(InvalidNodeKey::BadChar('[')));
        assert_eq!(
            NodeKey::parse("x".repeat(NodeKey::MAX_LEN + 1)),
            Err(InvalidNodeKey::TooLong(NodeKey::MAX_LEN + 1))
        );
    }
}
