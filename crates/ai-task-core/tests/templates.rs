//! 新建任务时可选的模板（`web/src/lib/tasks/templates/*.json`）必须是合法的编排。
//!
//! 模板是写死在前端的 JSON，没有哪条保存路径会先校验它：坏掉的模板要等到有人
//! 选中它、点了保存、看到一个 422，才会被发现。

use std::path::Path;

#[test]
fn every_task_template_is_a_valid_dag() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/lib/tasks/templates");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("读模板目录") {
        let path = entry.expect("读目录项").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("读模板");
        // 反序列化本身就是一道校验：DagSpec 拒绝未知字段，拼错一个键这里就会炸
        let spec: ai_task_proto::DagSpec = match serde_json::from_str(&text) {
            Ok(spec) => spec,
            Err(err) => panic!("{} 不是合法的 DagSpec：{err}", path.display()),
        };
        if let Err(errors) = ai_task_core::ValidatedDag::validate(spec) {
            panic!("{} 校验不过：{errors:?}", path.display());
        }
        checked += 1;
    }
    assert!(
        checked >= 2,
        "模板目录里应该至少有两个模板，实际 {checked} 个"
    );
}
