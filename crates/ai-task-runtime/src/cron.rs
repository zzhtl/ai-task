//! cron 表达式求值。
//!
//! 库选型见 `docs/adr/0005-cron-library.md`。这个模块把 croner 关在后面，
//! 对上只暴露「下一个触发点」和「错过了哪些」两个问题的答案。
//!
//! 时区是**必需参数**，不是可选装饰：`0 2 * * *` 在 `Asia/Shanghai` 和
//! `America/New_York` 是完全不同的时刻，而且后者一年里有一天没有 02:00、
//! 另一天有两个 01:00。用固定 offset 代替时区名，这两天就会出错。

use ai_task_proto::MisfirePolicy;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;

/// 补偿时最多往回追多少个触发点。
///
/// 一个 `*/1 * * * *` 的任务停机一周会错过一万个触发点。没有这个上限，
/// `fire_all` 会在恢复的瞬间灌进上万个 run，把集群打死——那不是「补数据」，
/// 那是自伤。超过上限时只补最近的这些，并在日志里说清丢了多少。
const MAX_CATCH_UP: usize = 512;

/// 一条已解析的定时规则。
#[derive(Debug, Clone)]
pub struct CronSchedule {
    cron: Cron,
    tz: Tz,
    expr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CronError {
    #[error("cron 表达式 `{expr}` 无法解析：{detail}")]
    BadExpression { expr: String, detail: String },

    #[error("`{0}` 不是合法的 IANA 时区名（形如 Asia/Shanghai、UTC）")]
    BadTimezone(String),

    #[error("cron 表达式 `{0}` 永远不会触发")]
    NeverFires(String),
}

impl CronSchedule {
    /// 解析表达式与时区。
    ///
    /// 支持 5 段（分 时 日 月 周）和 6 段（秒 分 时 日 月 周），
    /// 以及 `L`（月末）、`#`（第 n 个星期几）扩展。
    pub fn parse(expr: &str, timezone: &str) -> Result<Self, CronError> {
        let tz: Tz = timezone
            .parse()
            .map_err(|_| CronError::BadTimezone(timezone.to_string()))?;
        let cron: Cron = expr.parse().map_err(|err| CronError::BadExpression {
            expr: expr.to_string(),
            detail: format!("{err}"),
        })?;

        let schedule = Self {
            cron,
            tz,
            expr: expr.to_string(),
        };
        // 立刻验一次能不能算出下一个触发点。`0 0 30 2 *`（2 月 30 日）这种
        // 语法合法但永远不触发的表达式，必须在保存时就拒掉，而不是让用户
        // 等一年才发现任务从来没跑过。
        if schedule.next_after(Utc::now()).is_none() {
            return Err(CronError::NeverFires(expr.to_string()));
        }
        Ok(schedule)
    }

    /// `after` **之后**的第一个触发点（不含 `after` 本身）。
    #[must_use]
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local = after.with_timezone(&self.tz);
        self.cron
            .find_next_occurrence(&local, false)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    /// 枚举 `after`（不含）到 `now`（含）之间的所有触发点。
    ///
    /// 数量超过 [`MAX_CATCH_UP`] 时只保留**最近的**那些并置 `truncated`：
    /// 一个 `*/1 * * * *` 的任务停机一周会攒下一万个触发点，
    /// 不设上限的 `fire_all` 会在恢复瞬间灌爆集群——那不是补数据，是自伤。
    #[must_use]
    pub fn due_between(&self, after: DateTime<Utc>, now: DateTime<Utc>) -> DueSet {
        let mut points = Vec::new();
        let mut cursor = after;
        let mut total = 0usize;

        while let Some(next) = self.next_after(cursor) {
            if next > now {
                break;
            }
            total += 1;
            if points.len() == MAX_CATCH_UP {
                points.remove(0);
            }
            points.push(next);
            cursor = next;
        }

        DueSet {
            truncated: total > points.len(),
            total,
            points,
        }
    }

    #[must_use]
    pub fn expression(&self) -> &str {
        &self.expr
    }

    #[must_use]
    pub fn timezone(&self) -> Tz {
        self.tz
    }

    /// 把 UTC 时刻渲染成这条规则所在时区的本地时间，用于界面展示。
    #[must_use]
    pub fn to_local_string(&self, at: DateTime<Utc>) -> String {
        at.with_timezone(&self.tz)
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string()
    }
}

/// 一段区间内的到期触发点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueSet {
    /// 到期的触发点，按时间升序。被截断时只含最近的那些。
    pub points: Vec<DateTime<Utc>>,
    /// 区间内一共有多少个触发点（含被截断掉的）。
    pub total: usize,
    /// 数量超过上限，只保留了最近的那些。
    pub truncated: bool,
}

impl DueSet {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// 游标应当推进到的位置。`None` 表示这段区间内没有触发点。
    #[must_use]
    pub fn newest(&self) -> Option<DateTime<Utc>> {
        self.points.last().copied()
    }

    /// 按 misfire 策略挑出真正要跑的触发点。
    ///
    /// 「迟到」的判定用 `grace`：晚于它的算错过，之内的算准点。
    /// **准点的触发点在任何策略下都要跑**——否则 `skip` 在正常运行时会
    /// 退化成「永远不触发」，那不是用户想要的。
    ///
    /// 语义（与 Quartz 的 misfire 指令一致）：
    /// - `Skip`：只跑准点的，积压的全丢
    /// - `FireOnce`：不管积压多少，**总共只跑一个**（最近的那个）——
    ///   用户要的是「现在补跑一次」，不是「用三天前的上下文跑一次」
    /// - `FireAll`：全跑
    #[must_use]
    pub fn select(
        &self,
        now: DateTime<Utc>,
        policy: MisfirePolicy,
        grace: chrono::Duration,
    ) -> Vec<DateTime<Utc>> {
        match policy {
            MisfirePolicy::Skip => self
                .points
                .iter()
                .copied()
                .filter(|p| now - *p <= grace)
                .collect(),
            MisfirePolicy::FireOnce => self.newest().into_iter().collect(),
            MisfirePolicy::FireAll => self.points.clone(),
        }
    }
}

/// 给触发点加一个 `[0, jitter_s]` 内的随机延迟。
///
/// 只影响**领取时刻**，不影响写进 `runs.fire_at` 的规范触发点——后者是
/// `UNIQUE (schedule_id, fire_at)` 的一半，加了随机量幂等就废了。
#[must_use]
pub fn apply_jitter(fire_at: DateTime<Utc>, jitter_s: u32) -> DateTime<Utc> {
    if jitter_s == 0 {
        return fire_at;
    }
    let offset = rand::random_range(0..=i64::from(jitter_s));
    fire_at + chrono::Duration::seconds(offset)
}

/// 本地时间字符串 → UTC，用于测试和从配置读初值。
///
/// DST 的重复时刻取**较早**的那个，跳过的时刻向后对齐到存在的最近时刻。
#[must_use]
pub fn local_to_utc(tz: Tz, naive: chrono::NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earlier, _) => Some(earlier.with_timezone(&Utc)),
        // 春跳被抹掉的那一小时：往后找到第一个存在的时刻
        chrono::LocalResult::None => (1..=120)
            .find_map(|m| {
                tz.from_local_datetime(&(naive + chrono::Duration::minutes(m)))
                    .earliest()
            })
            .map(|t| t.with_timezone(&Utc)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn tz(name: &str) -> Tz {
        name.parse().expect("时区名")
    }

    fn at(zone: &str, s: &str) -> DateTime<Utc> {
        let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").expect("时间格式");
        local_to_utc(tz(zone), naive).expect("本地时间可转换")
    }

    /// 求 `from` 之后的 n 个触发点，渲染成本地时间。
    fn nexts(expr: &str, zone: &str, from: &str, n: usize) -> Vec<String> {
        let s = CronSchedule::parse(expr, zone).expect("解析");
        let mut out = Vec::new();
        let mut cursor = at(zone, from);
        for _ in 0..n {
            let next = s.next_after(cursor).expect("有下一个触发点");
            out.push(s.to_local_string(next));
            cursor = next;
        }
        out
    }

    #[test]
    fn accepts_both_five_and_six_field_expressions() {
        assert_eq!(
            nexts("*/1 * * * *", "Asia/Shanghai", "2026-09-08 10:00:00", 2),
            ["2026-09-08 10:01:00 CST", "2026-09-08 10:02:00 CST"]
        );
        assert_eq!(
            nexts("*/30 * * * * *", "Asia/Shanghai", "2026-09-08 10:00:00", 2),
            ["2026-09-08 10:00:30 CST", "2026-09-08 10:01:00 CST"]
        );
    }

    #[test]
    fn supports_last_day_of_month() {
        // 1 月 31 天、2 月 28 天（2026 不是闰年）、3 月 31 天
        assert_eq!(
            nexts("0 0 L * *", "Asia/Shanghai", "2026-01-15 00:00:00", 3),
            [
                "2026-01-31 00:00:00 CST",
                "2026-02-28 00:00:00 CST",
                "2026-03-31 00:00:00 CST"
            ]
        );
    }

    #[test]
    fn supports_nth_weekday() {
        // 2026-01-05 是第一个周一，01-12 是第二个
        assert_eq!(
            nexts("0 0 * * 1#2", "Asia/Shanghai", "2026-01-01 00:00:00", 2),
            ["2026-01-12 00:00:00 CST", "2026-02-09 00:00:00 CST"]
        );
    }

    #[test]
    fn leap_day_skips_non_leap_years() {
        assert_eq!(
            nexts("0 0 29 2 *", "Asia/Shanghai", "2026-03-01 00:00:00", 2),
            ["2028-02-29 00:00:00 CST", "2032-02-29 00:00:00 CST"]
        );
    }

    /// 2026-03-08 America/New_York 的 02:00–03:00 整个不存在。
    /// `30 2 * * *` 这天没有对应时刻，必须有个明确的落点，不能悄悄跳过一天。
    #[test]
    fn dst_spring_forward_lands_on_the_resumed_hour() {
        assert_eq!(
            nexts("30 2 * * *", "America/New_York", "2026-03-07 12:00:00", 2),
            ["2026-03-08 03:00:00 EDT", "2026-03-09 02:30:00 EDT"]
        );
    }

    /// 2026-11-01 America/New_York 的 01:00–02:00 出现两次（EDT 一次、EST 一次）。
    /// 必须只触发一次，否则每年秋天都会莫名多跑一个 run。
    #[test]
    fn dst_fall_back_fires_once_not_twice() {
        let s = CronSchedule::parse("30 1 * * *", "America/New_York").expect("解析");
        let mut cursor = at("America/New_York", "2026-10-31 12:00:00");
        let mut fires = Vec::new();
        for _ in 0..3 {
            let next = s.next_after(cursor).expect("有下一个");
            fires.push(next);
            cursor = next;
        }
        let on_nov_1: Vec<_> = fires
            .iter()
            .filter(|t| {
                use chrono::Datelike as _;
                t.with_timezone(&tz("America/New_York")).date_naive().day() == 1
            })
            .collect();
        assert_eq!(on_nov_1.len(), 1, "秋回当天只该触发一次：{fires:?}");
        assert_eq!(s.to_local_string(fires[0]), "2026-11-01 01:30:00 EDT");
    }

    #[test]
    fn timezone_actually_changes_the_instant() {
        // 同一个表达式在两个时区算出的 UTC 时刻必须不同，
        // 否则说明时区参数根本没生效
        let sh = CronSchedule::parse("0 2 * * *", "Asia/Shanghai").expect("解析");
        let ny = CronSchedule::parse("0 2 * * *", "America/New_York").expect("解析");
        let from = at("UTC", "2026-06-01 00:00:00");
        assert_ne!(sh.next_after(from), ny.next_after(from));
    }

    #[test]
    fn rejects_bad_expressions_and_timezones_at_parse_time() {
        assert!(matches!(
            CronSchedule::parse("not a cron", "UTC"),
            Err(CronError::BadExpression { .. })
        ));
        assert!(matches!(
            CronSchedule::parse("0 0 * * *", "Mars/Olympus"),
            Err(CronError::BadTimezone(_))
        ));
        // 语法合法但永远不触发：不能等用户过一年才发现任务没跑过
        assert!(matches!(
            CronSchedule::parse("0 0 30 2 *", "UTC"),
            Err(CronError::NeverFires(_))
        ));
    }

    const GRACE: chrono::Duration = chrono::Duration::seconds(30);

    /// 正常运行：每个 tick 只有一个准点触发点，**所有策略都必须跑它**。
    /// 这条挡住「skip 在正常运行时退化成永不触发」这个最容易犯的错。
    #[test]
    fn an_on_time_occurrence_fires_under_every_policy() {
        let s = CronSchedule::parse("*/1 * * * *", "UTC").expect("解析");
        let now = at("UTC", "2026-09-08 10:01:00") + chrono::Duration::milliseconds(300);
        let due = s.due_between(at("UTC", "2026-09-08 10:00:00"), now);
        assert_eq!(due.points, vec![at("UTC", "2026-09-08 10:01:00")]);

        for policy in [
            MisfirePolicy::Skip,
            MisfirePolicy::FireOnce,
            MisfirePolicy::FireAll,
        ] {
            assert_eq!(
                due.select(now, policy, GRACE).len(),
                1,
                "{policy:?} 在正常运行时也必须触发"
            );
        }
    }

    #[test]
    fn skip_discards_the_backlog_but_keeps_the_on_time_one() {
        // 停机三分钟后在 10:03:20 恢复：10:01 / 10:02 是积压，10:03 还在宽限期内
        let s = CronSchedule::parse("*/1 * * * *", "UTC").expect("解析");
        let now = at("UTC", "2026-09-08 10:03:20");
        let due = s.due_between(at("UTC", "2026-09-08 10:00:00"), now);
        assert_eq!(due.points.len(), 3);

        assert_eq!(
            due.select(now, MisfirePolicy::Skip, GRACE),
            vec![at("UTC", "2026-09-08 10:03:00")]
        );
    }

    #[test]
    fn skip_fires_nothing_when_even_the_newest_is_stale() {
        // 停机很久，最近的触发点也早过了宽限期：skip 就是一个都不补
        let s = CronSchedule::parse("0 * * * *", "UTC").expect("解析");
        let now = at("UTC", "2026-09-08 13:45:00");
        let due = s.due_between(at("UTC", "2026-09-08 10:00:00"), now);
        assert!(due.select(now, MisfirePolicy::Skip, GRACE).is_empty());
    }

    #[test]
    fn fire_once_collapses_the_whole_backlog_into_exactly_one_run() {
        let s = CronSchedule::parse("0 * * * *", "UTC").expect("解析");
        let now = at("UTC", "2026-09-08 13:30:00");
        let due = s.due_between(at("UTC", "2026-09-08 10:00:00"), now);
        assert_eq!(due.points.len(), 3, "11:00 / 12:00 / 13:00");

        let fires = due.select(now, MisfirePolicy::FireOnce, GRACE);
        assert_eq!(fires.len(), 1, "不管积压多少，fire_once 总共只跑一个");
        // 补最近的而不是最早的：用户要的是"现在补跑一次"
        assert_eq!(fires[0], at("UTC", "2026-09-08 13:00:00"));
    }

    #[test]
    fn fire_all_replays_every_missed_point() {
        let s = CronSchedule::parse("0 * * * *", "UTC").expect("解析");
        let now = at("UTC", "2026-09-08 13:30:00");
        let due = s.due_between(at("UTC", "2026-09-08 10:00:00"), now);
        assert_eq!(
            due.select(now, MisfirePolicy::FireAll, GRACE),
            vec![
                at("UTC", "2026-09-08 11:00:00"),
                at("UTC", "2026-09-08 12:00:00"),
                at("UTC", "2026-09-08 13:00:00"),
            ]
        );
    }

    #[test]
    fn enumeration_is_bounded_so_a_long_outage_cannot_flood_the_cluster() {
        // 每分钟一次停机一周 = 一万个触发点。不设上限 fire_all 会灌爆集群。
        let s = CronSchedule::parse("* * * * *", "UTC").expect("解析");
        let now = at("UTC", "2026-09-08 00:00:00");
        let due = s.due_between(at("UTC", "2026-09-01 00:00:00"), now);

        assert!(due.truncated);
        assert_eq!(due.points.len(), MAX_CATCH_UP);
        assert_eq!(due.total, 7 * 24 * 60, "总数要如实报出来，不能被截断掩盖");
        // 截断后保留的必须是最近的那些
        assert_eq!(due.newest(), Some(now));
    }

    #[test]
    fn an_empty_interval_yields_nothing_to_realign() {
        let s = CronSchedule::parse("0 0 * * *", "UTC").expect("解析");
        let due = s.due_between(
            at("UTC", "2026-09-08 00:00:00"),
            at("UTC", "2026-09-08 10:00:00"),
        );
        assert!(due.is_empty());
        assert_eq!(due.newest(), None);
        assert_eq!(due.total, 0);
    }

    #[test]
    fn jitter_only_delays_and_stays_within_bounds() {
        let base = at("UTC", "2026-09-08 10:00:00");
        assert_eq!(apply_jitter(base, 0), base, "抖动为 0 时必须原样返回");
        for _ in 0..200 {
            let jittered = apply_jitter(base, 30);
            let delay = (jittered - base).num_seconds();
            assert!((0..=30).contains(&delay), "抖动 {delay}s 越界");
        }
    }
}
