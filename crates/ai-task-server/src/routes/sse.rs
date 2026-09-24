//! Run 事件的 SSE 推送。
//!
//! 用 SSE 而不是 WebSocket：这条链路本来就是单向的，而 SSE 自带重连和
//! `Last-Event-ID`，正好对上事件日志的 seq。取消、审批这些反向动作走普通 POST。
//!
//! 续传语义：客户端带 `Last-Event-ID: <seq>`（浏览器 `EventSource` 会自动带上
//! 它收到的最后一个 `id:`），服务端从 `seq + 1` 开始补发。先把历史补完再转实时，
//! 因此不会重复也不会丢。

use std::convert::Infallible;
use std::time::Duration;

use ai_task_proto::RunId;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;

use crate::error::AppError;
use crate::extract::Path;
use crate::state::AppState;

/// 一次补读的上限。历史很长时分多轮补，避免一次查询把内存打满。
const CATCHUP_BATCH: i64 = 500;

/// 兜底轮询间隔。
///
/// 正常情况下事件靠唤醒提示推送，这个只在提示丢了（NOTIFY 不保证送达、
/// 连接重连期间等）时兜底。设得比刷新间隔宽松，免得白白打数据库。
const FALLBACK_POLL: Duration = Duration::from_secs(2);

/// SSE 心跳间隔。
///
/// 中间的反向代理通常会掐掉一段时间无数据的连接，心跳让连接活着。
/// 同时最多挂多少条 SSE。
///
/// 每条连接是一个不限时长的任务，拿着一个 broadcast 接收端，
/// 而且每隔 FALLBACK_POLL 就要占一次连接池。之前没有任何上限：
/// 开着几百个标签页就能把池子耗干，而池子耗干表现为**所有接口**一起变慢。
const MAX_STREAMS: usize = 64;

static STREAMS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS)));

const KEEPALIVE: Duration = Duration::from_secs(15);

/// `GET /api/v1/runs/{id}/events`
pub async fn stream(
    State(state): State<AppState>,
    Path(run_id): Path<RunId>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    // 先确认 run 存在且属于当前 workspace，再开流。
    // 否则不存在的 id 会挂出一条永远没数据的连接。
    let run = state
        .store
        .get_run(state.workspace_id, run_id)
        .await
        .map_err(|err| map_not_found(err, "run", run_id))?;

    // 拿一个名额。拿不到就让客户端稍后再来——EventSource 自己会重连。
    let Ok(permit) = std::sync::Arc::clone(&STREAMS).try_acquire_owned() else {
        return Err(AppError::Conflict("同时打开的事件流太多，稍后再试".into()));
    };

    let after_seq = last_event_id(&headers).unwrap_or(0);
    // 这个 run 的事件不可能早于它自己被创建的时刻。给查询一个 ts 下界，
    // 就能让 Postgres 裁掉所有更早的月度分区——run 已经在手上了，不用额外查一次。
    // 留一分钟余量，免得被时钟回拨这类事情卡掉边界上的事件。
    let not_before = Some(run.created_at - chrono::Duration::minutes(1));
    let store = state.store.clone();
    let mut wakeups = state.bus.subscribe();
    let mut cursor = after_seq;
    let mut terminal = run.status.is_terminal() && cursor >= run.max_seq;

    let stream = async_stream::stream! {
        // 名额跟着流走：流结束（正常收尾或客户端断开）时自动还回去
        let _permit = permit;
        loop {
            if terminal {
                break;
            }

            // 先把落后的补完，再等下一次唤醒
            loop {
                let batch = match store
                    .read_events_after(run_id, cursor, CATCHUP_BATCH, not_before)
                    .await
                {
                    Ok(batch) => batch,
                    Err(err) => {
                        tracing::warn!(%run_id, error = %err, "读取事件失败，本轮跳过");
                        break;
                    }
                };
                if batch.is_empty() {
                    break;
                }
                let last = batch.len() == usize::try_from(CATCHUP_BATCH).unwrap_or(usize::MAX);
                for event in batch {
                    cursor = event.seq;
                    if matches!(&event.body, ai_task_proto::RunEventBody::RunFinished { .. }) {
                        terminal = true;
                    }
                    match Event::default().id(event.seq.to_string()).json_data(&event) {
                        Ok(sse) => yield Ok(sse),
                        Err(err) => {
                            // 序列化不了说明事件体本身有问题，跳过这一条也要继续，
                            // 不能让一条坏事件把整个流打断
                            tracing::error!(%run_id, seq = event.seq, error = %err, "事件序列化失败");
                        }
                    }
                }
                if !last {
                    break;
                }
            }

            if terminal {
                break;
            }

            // 等唤醒；超时就兜底轮询一次。
            //
            // **别的 run 的提示必须在这里就地丢掉。**
            // 之前这一档写的是 `continue`，注释也写着"忽略"——但 continue 回到的是
            // **外层**循环，于是立刻又发一次 read_events_after。
            // K 个连接 × 一个忙碌的 run 每秒 flush 几次 = 每秒几千次无谓查询，
            // 而连接池只有 4–32。
            let mut closed = false;
            loop {
                match tokio::time::timeout(FALLBACK_POLL, wakeups.recv()).await {
                    // 本 run 的提示，或者超时兜底：跳出去补读
                    Ok(Ok(wakeup)) if wakeup.run_id == run_id => break,
                    Err(_) => break,
                    // 别的 run 的提示：接着等，不去碰数据库
                    Ok(Ok(_)) => {}
                    // 落后太多：不用管，补读会按 cursor 把差的都取回来
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                        tracing::debug!(%run_id, skipped = n, "订阅落后，靠补读追上");
                        break;
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                        closed = true;
                        break;
                    }
                }
            }
            if closed {
                break;
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE)))
}

/// 浏览器的 `EventSource` 重连时会带上它收到的最后一个 `id:`。
fn last_event_id(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("last-event-id")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|seq: &i64| *seq >= 0)
}

fn map_not_found(err: ai_task_store::StoreError, what: &str, id: RunId) -> AppError {
    match err {
        ai_task_store::StoreError::NotFound { .. } => {
            AppError::NotFound(format!("{what} {id} 不存在"))
        }
        other => AppError::Store(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("last-event-id", HeaderValue::from_str(value).expect("头值"));
        h
    }

    #[test]
    fn resumes_from_the_last_event_id() {
        assert_eq!(last_event_id(&headers("42")), Some(42));
        assert_eq!(last_event_id(&headers(" 42 ")), Some(42));
    }

    #[test]
    fn a_garbage_last_event_id_falls_back_to_a_full_replay() {
        // 解析不了就当没带，从头补 —— 重复总好过丢事件
        assert_eq!(last_event_id(&HeaderMap::new()), None);
        assert_eq!(last_event_id(&headers("abc")), None);
        assert_eq!(last_event_id(&headers("-1")), None);
        assert_eq!(last_event_id(&headers("")), None);
    }

    #[test]
    fn keepalive_is_shorter_than_common_proxy_idle_timeouts() {
        // nginx 默认 proxy_read_timeout 是 60s
        assert!(KEEPALIVE < Duration::from_secs(60));
    }
}
