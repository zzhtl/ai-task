//! 集成测试的共用前置检查。
//!
//! 这个 crate 的三个测试二进制各自带着自己的夹具（它们要的 store / 引擎 / agent
//! 组合各不相同），但「前置条件不满足时该怎么办」必须只有一个答案。

#![allow(dead_code)]

/// 没配前置条件时：该跳过，还是该失败？
///
/// 在日志里「跳过」和「通过」长得一模一样。本地开发跳过是方便，CI 里跳过是自欺——
/// 一个没连数据库、没建 agent 的 `cargo test --workspace` 会全绿，而它什么都没验证。
/// 所以 `CI` 或 `AI_TASK_REQUIRE_DB_TESTS` 在场时，缺前置条件一律硬失败。
pub fn skip_or_fail(name: &str, why: &str) {
    assert!(
        std::env::var_os("CI").is_none() && std::env::var_os("AI_TASK_REQUIRE_DB_TESTS").is_none(),
        "{name}：{why}。设了 CI / AI_TASK_REQUIRE_DB_TESTS 就不允许跳过——\
         否则「测试全绿」不代表测试跑过。"
    );
    eprintln!("跳过 {name}：{why}");
}
