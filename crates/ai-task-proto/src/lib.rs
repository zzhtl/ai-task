//! 前后端共享的线上类型。
//!
//! 这个 crate 是 API 契约的唯一来源：Rust 侧直接用，TypeScript 侧由 `ts-rs`
//! 导出（`cargo test -p ai-task-proto` 生成到 `web/src/lib/api/types/`）。
//!
//! 为什么不像仓库里其它项目那样手写 TS 镜像：[`event::RunEventBody`] 一个枚举就有
//! 18 个 variant，两侧字段必须逐个对齐，而漂移的表现是「实时监控界面某类事件静默
//! 不显示」，单元测试抓不到。类型基数够大时，生成比手写便宜。
//!
//! 契约约定（对应 `docs/adr/0002-api-contract.md`）：
//! - 时间一律 RFC 3339 带 offset（`DateTime<Utc>` → `2026-09-08T02:15:30Z`）
//! - ID 对客户端是不透明字符串，不参与运算
//! - 金额是十进制字符串，绝不用浮点（见 [`money::UsdMicros`]）
//! - 枚举是字符串，**客户端必须容忍未知值**（新增 variant 是兼容变更）
//! - 字段命名统一 snake_case

pub mod dto;
pub mod event;
pub mod ids;
pub mod money;
pub mod spec;

pub use dto::*;
pub use event::*;
pub use ids::*;
pub use money::*;
pub use spec::*;
