//! 将 u8 以“百分之一”的定点数形式与 JSON 浮点数互转（跨协议共享）。
//!
//! 内部始终是 u8（0-200 表示 0.0-2.0），序列化时直接用整数运算拼出十进制字符串
//! 再作为 JSON number 输出，全程不经过 f32/f64，既无浮点误差也无精度损失。
//! 例如 100 → `1`，70 → `0.7`，150 → `1.5`。

use serde::de::{Error, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

/// 把 `value`（百分之一单位）拼成十进制字符串，如 70 → "0.7"、100 → "1"。
fn to_decimal_string(value: u8) -> String {
    let int = value / 100;
    let frac = value % 100;
    if frac == 0 {
        int.to_string()
    } else if frac.is_multiple_of(10) {
        format!("{int}.{}", frac / 10)
    } else {
        format!("{int}.{frac:02}")
    }
}

pub fn serialize<S: Serializer>(value: &u8, serializer: S) -> Result<S::Ok, S::Error> {
    // RawValue 让拼好的十进制字符串原样作为 JSON number 输出，不再走浮点。
    let raw =
        RawValue::from_string(to_decimal_string(*value)).map_err(serde::ser::Error::custom)?;
    raw.serialize(serializer)
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    // 反序列化仅在读回请求体时用到；用 f64 解析后换算回 u8。
    let v = f64::deserialize(deserializer)?;
    if !(0.0..=2.0).contains(&v) {
        return Err(Error::invalid_value(
            Unexpected::Float(v),
            &"a number in 0.0..=2.0",
        ));
    }
    Ok((v * 100.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super as percent;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct WithPercent {
        #[serde(with = "percent")]
        value: u8,
    }

    #[test]
    fn percent_serialize_decimal_string() {
        let cases = [(0, "0"), (5, "0.05"), (70, "0.7"), (100, "1"), (150, "1.5")];
        for (input, expected) in cases {
            let json = serde_json::to_string(&WithPercent { value: input }).unwrap();
            assert_eq!(json, format!("{{\"value\":{expected}}}"), "input {input}");
        }
    }

    #[test]
    fn percent_deserialize_roundtrip() {
        let w: WithPercent = serde_json::from_str("{\"value\":0.7}").unwrap();
        assert_eq!(w.value, 70);
        let w: WithPercent = serde_json::from_str("{\"value\":1}").unwrap();
        assert_eq!(w.value, 100);
    }

    #[test]
    fn percent_deserialize_out_of_range() {
        assert!(serde_json::from_str::<WithPercent>("{\"value\":2.5}").is_err());
        assert!(serde_json::from_str::<WithPercent>("{\"value\":-0.1}").is_err());
    }
}
