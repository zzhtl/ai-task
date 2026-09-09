//! `stream-json` 的防腐层。
//!
//! 这里是整个系统对 Claude Code CLI 的**唯一**认知点。CLI 的事件形状不是稳定
//! 契约，所以约束只有两条：
//!
//! 1. **任何输入都不能让执行挂掉。** 不认识的事件类型、解析不了的行、缺字段，
//!    一律降级成 [`ExecEvent::Warning`] 继续走。CLI 升级加了个新事件类型，
//!    不该让凌晨两点的定时任务失败。
//! 2. **只信 `result` 事件里的用量。** 见下面 `finish()` 的注释——这是实测出来
//!    的，不是猜的。
//!
//! 回归基线是 `tests/fixtures/` 下真实录制的 JSONL。升级 CLI 后重录一份、
//! 跑一遍测试，就知道要不要改这一层。

use ai_task_proto::UsdMicros;
use serde_json::Value;

use crate::event::{ExecEvent, ExecOutcome, ToolOutcome};

/// 工具输出摘要的长度上限。全文不进事件日志。
const PREVIEW_LIMIT: usize = 400;

/// 逐行解码 `stream-json`。
///
/// 无状态：CLI 每行都是自包含的一条事件。保留结构体是为了将来需要跨行状态时
/// 不用改调用方签名。
#[derive(Debug, Default)]
pub struct Decoder {
    /// 已经发过 `Finished` 了。之后的行只可能是噪音。
    finished: bool,
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否已经解出终态。
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// 解一行。一行可能产出零到多条事件（一条 assistant 事件里可以有多个内容块）。
    pub fn line(&mut self, line: &str) -> Vec<ExecEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }

        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(err) => {
                // CLI 偶尔会往 stdout 混进非 JSON（比如告警）。截断后记下来，
                // 别把整行原样塞进事件日志。
                return vec![ExecEvent::Warning {
                    message: format!("stream-json 解析失败（{err}）：{}", truncate(line, 200)),
                }];
            }
        };

        match value.get("type").and_then(Value::as_str) {
            Some("system") => self.system(&value),
            Some("assistant") => assistant(&value),
            Some("user") => user(&value),
            Some("result") => self.finish(&value),
            // 限流不是错误，但它解释了「为什么这次特别慢」，值得留痕
            Some("rate_limit_event") => rate_limit(&value),
            Some(other) => vec![ExecEvent::Warning {
                message: format!("stream-json 出现未知事件类型 `{other}`，已忽略"),
            }],
            None => vec![ExecEvent::Warning {
                message: format!("stream-json 事件缺少 type 字段：{}", truncate(line, 200)),
            }],
        }
    }

    fn system(&mut self, value: &Value) -> Vec<ExecEvent> {
        match value.get("subtype").and_then(Value::as_str) {
            Some("init") => vec![ExecEvent::Started {
                session_id: string_at(value, "session_id").unwrap_or_default(),
                model: string_at(value, "model").unwrap_or_default(),
                tools: value
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                cli_version: string_at(value, "claude_code_version"),
            }],
            Some("permission_denied") => vec![ExecEvent::PermissionDenied {
                tool_use_id: string_at(value, "tool_use_id").unwrap_or_default(),
                tool: string_at(value, "tool_name").unwrap_or_default(),
                reason: string_at(value, "decision_reason")
                    .or_else(|| string_at(value, "message"))
                    .unwrap_or_else(|| "未给出原因".into()),
            }],
            // 思考 token 的进度心跳，纯噪音，一次执行能刷出几十条
            Some("thinking_tokens") => Vec::new(),
            Some(other) => vec![ExecEvent::Warning {
                message: format!("stream-json 出现未知 system 子类型 `{other}`，已忽略"),
            }],
            None => Vec::new(),
        }
    }

    /// `result` 事件 —— 一次执行唯一可信的用量来源。
    ///
    /// **为什么不逐条累加 `assistant` 事件里的 usage**：实测（见
    /// `tests/fixtures/hermetic-count-lines.jsonl`）同一次执行里，照单累加
    /// `output_tokens` 得 16，按 `message.id` 去重得 8，而 `result` 的权威值是
    /// **694**。逐条 usage 是流式过程中的局部快照，和最终计费差了两个数量级，
    /// 拿它算成本会让预算熔断形同虚设。
    ///
    /// **为什么 token 数取 `modelUsage` 而不是 `usage`**：预算熔断时 `usage`
    /// 整个是 0（见 `budget-exceeded.jsonl`），只有 `modelUsage` 和
    /// `total_cost_usd` 还是对的。取前者才能在两种终态下都拿到真实数字。
    fn finish(&mut self, value: &Value) -> Vec<ExecEvent> {
        self.finished = true;

        let cost = value
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .map_or(UsdMicros::ZERO, usd_from_f64);

        let mut events = Vec::with_capacity(2);
        let tokens = ModelTokens::from_model_usage(value.get("modelUsage"));
        events.push(ExecEvent::Usage {
            model: string_at(value, "model").unwrap_or_else(|| tokens.model.clone()),
            input_tokens: tokens.input,
            output_tokens: tokens.output,
            cache_read_tokens: tokens.cache_read,
            cache_creation_tokens: tokens.cache_creation,
            cost_usd: cost,
        });

        let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
        let terminal = value
            .get("terminal_reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        let is_error = value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let outcome = if subtype == "error_max_budget_usd" || terminal == "budget_exhausted" {
            ExecOutcome::BudgetExceeded
        } else if is_error || subtype.starts_with("error") {
            ExecOutcome::Failed {
                reason: string_at(value, "result")
                    .filter(|s| !s.is_empty())
                    .or_else(|| string_at(value, "api_error_status"))
                    .unwrap_or_else(|| {
                        format!("CLI 以 `{subtype}` 结束（terminal_reason={terminal}）")
                    }),
            }
        } else {
            ExecOutcome::Success {
                result: string_at(value, "result").unwrap_or_default(),
                turns: value
                    .get("num_turns")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
            }
        };
        events.push(ExecEvent::Finished(outcome));
        events
    }
}

fn assistant(value: &Value) -> Vec<ExecEvent> {
    let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else {
        return Vec::new();
    };

    blocks
        .iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str) {
            Some("thinking") => Some(ExecEvent::Thinking {
                text: string_at(block, "thinking")
                    .or_else(|| string_at(block, "text"))
                    .unwrap_or_default(),
            }),
            Some("text") => Some(ExecEvent::Text {
                text: string_at(block, "text").unwrap_or_default(),
            }),
            Some("tool_use") => Some(ExecEvent::ToolRequested {
                tool_use_id: string_at(block, "id").unwrap_or_default(),
                tool: string_at(block, "name").unwrap_or_default(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            // redacted_thinking 之类：知道有这么回事就行，内容本就不可读
            _ => None,
        })
        .collect()
}

fn user(value: &Value) -> Vec<ExecEvent> {
    // `.message` 有时是对象有时是裸字符串，后者不携带工具结果
    let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) else {
        return Vec::new();
    };

    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| ExecEvent::ToolCompleted {
            tool_use_id: string_at(block, "tool_use_id").unwrap_or_default(),
            outcome: if block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                ToolOutcome::Error
            } else {
                ToolOutcome::Ok
            },
            output_preview: preview(block.get("content")),
        })
        .collect()
}

fn rate_limit(value: &Value) -> Vec<ExecEvent> {
    let status = value
        .pointer("/rate_limit_info/status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    // allowed 是常态心跳，只有真被限了才值得记
    if status == "allowed" {
        return Vec::new();
    }
    vec![ExecEvent::Warning {
        message: format!("触发 API 限流（status={status}），本次执行会变慢"),
    }]
}

/// `tool_result.content` 可能是字符串，也可能是内容块数组。
fn preview(content: Option<&Value>) -> String {
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| string_at(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    truncate(&text, PREVIEW_LIMIT)
}

/// 汇总 `modelUsage` 里各模型的 token。
struct ModelTokens {
    model: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

impl ModelTokens {
    fn from_model_usage(usage: Option<&Value>) -> Self {
        let mut out = Self {
            model: String::new(),
            input: 0,
            output: 0,
            cache_read: 0,
            cache_creation: 0,
        };
        let Some(map) = usage.and_then(Value::as_object) else {
            return out;
        };
        // 同一次执行可能同时出现 `claude-haiku-4-5` 和带日期后缀的变体
        //（后者是内部小请求）。以 token 最多的那个作为展示用的模型名。
        let mut best_output = 0;
        for (name, entry) in map {
            let input = u64_at(entry, "inputTokens");
            let output = u64_at(entry, "outputTokens");
            out.input += input;
            out.output += output;
            out.cache_read += u64_at(entry, "cacheReadInputTokens");
            out.cache_creation += u64_at(entry, "cacheCreationInputTokens");
            if output >= best_output {
                best_output = output;
                out.model = entry
                    .get("canonicalModel")
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_string();
            }
        }
        out
    }
}

fn u64_at(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// 美元浮点转微美元。四舍五入，不截断——截断会让每次执行都少算一点。
fn usd_from_f64(usd: f64) -> UsdMicros {
    if !usd.is_finite() {
        return UsdMicros::ZERO;
    }
    let micros = (usd * 1_000_000.0).round();
    UsdMicros(micros.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
}

/// 按**字符**截断，不是字节——按字节切会把多字节字符劈成非法 UTF-8。
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}…（已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(line: &str) -> Vec<ExecEvent> {
        Decoder::new().line(line)
    }

    #[test]
    fn unknown_event_types_degrade_to_a_warning_instead_of_failing() {
        // CLI 升级加了个新事件类型，不能让凌晨两点的定时任务挂掉
        let events = decode(r#"{"type":"telemetry_v2","payload":{}}"#);
        assert!(matches!(events.as_slice(), [ExecEvent::Warning { .. }]));
        assert!(!events[0].is_terminal());
    }

    #[test]
    fn malformed_lines_are_warnings_and_do_not_echo_unbounded_text() {
        let junk = format!("not json {}", "x".repeat(5_000));
        let events = decode(&junk);
        let [ExecEvent::Warning { message }] = events.as_slice() else {
            panic!("应当是 Warning：{events:?}");
        };
        assert!(
            message.len() < 400,
            "告警文本要有界，实际 {} 字节",
            message.len()
        );
    }

    #[test]
    fn events_without_a_type_do_not_panic() {
        assert!(matches!(
            decode(r#"{"foo":1}"#).as_slice(),
            [ExecEvent::Warning { .. }]
        ));
        assert!(decode("   ").is_empty());
    }

    #[test]
    fn thinking_token_heartbeats_are_dropped() {
        // 一次执行能刷出几十条，进事件日志纯粹是噪音
        let events =
            decode(r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":12}"#);
        assert!(events.is_empty());
    }

    #[test]
    fn allowed_rate_limit_heartbeats_are_dropped_but_throttling_is_recorded() {
        assert!(
            decode(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#)
                .is_empty()
        );
        assert!(matches!(
            decode(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#)
                .as_slice(),
            [ExecEvent::Warning { .. }]
        ));
    }

    #[test]
    fn tool_result_content_may_be_a_string_or_a_block_array() {
        let as_string = decode(
            r#"{"type":"user","message":{"content":[
                 {"type":"tool_result","tool_use_id":"t1","content":"hello","is_error":false}]}}"#,
        );
        let as_blocks = decode(
            r#"{"type":"user","message":{"content":[
                 {"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"hello"}]}]}}"#,
        );
        for events in [as_string, as_blocks] {
            let [
                ExecEvent::ToolCompleted {
                    output_preview,
                    outcome,
                    ..
                },
            ] = events.as_slice()
            else {
                panic!("应当解出一条 ToolCompleted：{events:?}");
            };
            assert_eq!(output_preview, "hello");
            assert_eq!(*outcome, ToolOutcome::Ok);
        }
    }

    #[test]
    fn a_string_message_does_not_panic() {
        // `.message` 有时是裸字符串而不是对象
        assert!(decode(r#"{"type":"user","message":"plain text"}"#).is_empty());
    }

    #[test]
    fn tool_output_preview_is_truncated_on_char_boundaries() {
        let long = "中".repeat(1_000);
        let line = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type":"tool_result","tool_use_id":"t","content": long}]}
        })
        .to_string();
        let events = decode(&line);
        let [ExecEvent::ToolCompleted { output_preview, .. }] = events.as_slice() else {
            panic!("应当解出 ToolCompleted");
        };
        // 按字符截断而不是字节，否则多字节字符会被劈开
        assert_eq!(
            output_preview.chars().count(),
            PREVIEW_LIMIT + "…（已截断）".chars().count()
        );
    }

    #[test]
    fn cost_rounds_instead_of_truncating() {
        // 截断会让每次执行都少算一点，累积起来预算就守不住了
        assert_eq!(usd_from_f64(0.0335701), UsdMicros(33_570));
        assert_eq!(usd_from_f64(0.0000005), UsdMicros(1));
        assert_eq!(usd_from_f64(0.0), UsdMicros::ZERO);
        assert_eq!(usd_from_f64(f64::NAN), UsdMicros::ZERO);
        assert_eq!(usd_from_f64(f64::INFINITY), UsdMicros::ZERO);
    }

    #[test]
    fn decoder_reports_when_it_has_seen_a_terminal_event() {
        let mut decoder = Decoder::new();
        assert!(!decoder.is_finished());
        decoder.line(r#"{"type":"result","subtype":"success","is_error":false,"result":"4"}"#);
        assert!(decoder.is_finished());
    }
}
