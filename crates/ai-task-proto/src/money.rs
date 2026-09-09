//! 金额类型。
//!
//! 内部用 i64 微美元（1 USD = 1_000_000 µ$），线上是十进制字符串。
//!
//! 为什么不是浮点：成本要累加、要和预算比较、要落库，浮点在这三处都会出错。
//! 为什么不引 `rust_decimal`：定价的最小粒度就是「每 token 多少微美元」——
//! Haiku 4.5 输入 $1/MTok 正好是 1 µ$/token，整数可精确表示，不需要任意精度小数。
//! i64 上限约 $9.2 万亿，不可能溢出。

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use ts_rs::TS;

/// 微美元（10^-6 USD）。序列化为十进制字符串，如 `"0.042500"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash, TS)]
#[ts(export, type = "string")]
pub struct UsdMicros(pub i64);

impl UsdMicros {
    /// 1 USD 对应的微美元数。
    pub const PER_USD: i64 = 1_000_000;

    pub const ZERO: Self = Self(0);

    /// 饱和加法。成本累加不应该因为溢出而 panic 或回绕。
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// 按每百万 token 的美元单价，算 `tokens` 个 token 的成本。
    ///
    /// 先乘后除，避免单价先取整导致整批 token 成本归零。
    #[must_use]
    pub fn from_tokens(tokens: u64, usd_per_mtok: Self) -> Self {
        let micros = i128::from(usd_per_mtok.0) * i128::from(tokens) / 1_000_000;
        Self(micros.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
    }

    /// 渲染成定长 6 位小数的十进制字符串。
    #[must_use]
    pub fn to_decimal_string(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
    }

    /// 解析十进制字符串。接受任意位数的小数，超出 6 位的部分直接截断（不四舍五入）。
    pub fn parse_decimal(s: &str) -> Result<Self, ParseUsdError> {
        let s = s.trim();
        let (neg, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (int_part, frac_part) = match digits.split_once('.') {
            Some((i, f)) => (i, f),
            None => (digits, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(ParseUsdError::Empty);
        }
        if let Some(c) = int_part
            .chars()
            .chain(frac_part.chars())
            .find(|c| !c.is_ascii_digit())
        {
            return Err(ParseUsdError::BadChar(c));
        }
        let int_val: i64 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().map_err(|_| ParseUsdError::Overflow)?
        };
        let mut frac: i64 = 0;
        for i in 0..6 {
            frac = frac * 10 + i64::from(frac_part.as_bytes().get(i).map_or(0, |b| b - b'0'));
        }
        let micros = int_val
            .checked_mul(1_000_000)
            .and_then(|v| v.checked_add(frac))
            .ok_or(ParseUsdError::Overflow)?;
        Ok(Self(if neg { -micros } else { micros }))
    }
}

impl std::fmt::Display for UsdMicros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_decimal_string())
    }
}

/// [`UsdMicros::parse_decimal`] 的拒绝原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseUsdError {
    #[error("金额为空")]
    Empty,
    #[error("金额含非法字符 {0:?}")]
    BadChar(char),
    #[error("金额超出 i64 微美元可表示范围")]
    Overflow,
}

impl Serialize for UsdMicros {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_decimal_string())
    }
}

impl<'de> Deserialize<'de> for UsdMicros {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // 只接受字符串。允许数字会让 JS 客户端悄悄丢精度，正是这个类型要避免的。
        let s = String::deserialize(d)?;
        Self::parse_decimal(&s).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_fixed_six_decimals() {
        assert_eq!(UsdMicros(0).to_decimal_string(), "0.000000");
        assert_eq!(UsdMicros(1).to_decimal_string(), "0.000001");
        assert_eq!(UsdMicros(42_500).to_decimal_string(), "0.042500");
        assert_eq!(UsdMicros(1_000_000).to_decimal_string(), "1.000000");
        assert_eq!(UsdMicros(-1_500_000).to_decimal_string(), "-1.500000");
    }

    #[test]
    fn parse_round_trips() {
        for micros in [0, 1, 999_999, 1_000_000, 123_456_789, -42] {
            let v = UsdMicros(micros);
            assert_eq!(UsdMicros::parse_decimal(&v.to_decimal_string()), Ok(v));
        }
    }

    #[test]
    fn parse_accepts_shorthand_and_truncates_excess_precision() {
        assert_eq!(UsdMicros::parse_decimal("1"), Ok(UsdMicros(1_000_000)));
        assert_eq!(UsdMicros::parse_decimal(".5"), Ok(UsdMicros(500_000)));
        assert_eq!(UsdMicros::parse_decimal("0.05"), Ok(UsdMicros(50_000)));
        // 第 7 位及以后直接截断，不进位
        assert_eq!(UsdMicros::parse_decimal("0.0000019"), Ok(UsdMicros(1)));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(UsdMicros::parse_decimal(""), Err(ParseUsdError::Empty));
        assert_eq!(
            UsdMicros::parse_decimal("1.2.3"),
            Err(ParseUsdError::BadChar('.'))
        );
        assert_eq!(
            UsdMicros::parse_decimal("1e5"),
            Err(ParseUsdError::BadChar('e'))
        );
    }

    #[test]
    fn json_is_a_string_not_a_number() {
        let json = serde_json::to_string(&UsdMicros(42_500)).expect("序列化");
        assert_eq!(json, "\"0.042500\"");
        // 数字形式必须被拒绝：JS 的 Number 会悄悄丢精度
        assert!(serde_json::from_str::<UsdMicros>("0.0425").is_err());
    }

    #[test]
    fn token_cost_does_not_round_the_unit_price_to_zero() {
        // Haiku 4.5 输入 $1/MTok：1000 token = $0.001
        let cost = UsdMicros::from_tokens(1_000, UsdMicros(1_000_000));
        assert_eq!(cost, UsdMicros(1_000));
        // Opus 5 输出 $25/MTok：3 token = $0.000075
        let cost = UsdMicros::from_tokens(3, UsdMicros(25_000_000));
        assert_eq!(cost, UsdMicros(75));
    }

    #[test]
    fn accumulation_saturates_instead_of_wrapping() {
        assert_eq!(
            UsdMicros(i64::MAX).saturating_add(UsdMicros(1)),
            UsdMicros(i64::MAX)
        );
    }
}
