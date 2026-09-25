//! Lenient number deserialisation.
//!
//! Databricks services don't encode numbers consistently:
//! - a 64-bit integer may arrive as `123` or `"123"` (databricks-sdk-go #1808);
//! - a float may arrive as `"NaN"`, `"Infinity"` or `"-Infinity"`, the proto3
//!   JSON spelling of non-finite values (databricks-sdk-go #1498).
//!
//! Generated models use these with `#[serde(deserialize_with = "…")]` on
//! every integer and float field. Serialisation is unchanged.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::Deserializer;

#[derive(Deserialize)]
#[serde(untagged)]
enum IntOrString {
    Int(i64),
    Str(String),
    // Integral values sent as floats (`12.0`).
    Float(f64),
}

impl IntOrString {
    fn into_i64<E: serde::de::Error>(self) -> Result<i64, E> {
        match self {
            Self::Int(i) => Ok(i),
            Self::Str(s) => s
                .trim()
                .parse()
                .map_err(|_| E::custom(format!("invalid integer string {s:?}"))),
            #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
            Self::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Ok(f as i64),
            Self::Float(f) => Err(E::custom(format!("non-integral number {f}"))),
        }
    }
}

/// A required integer; `null` becomes `0`.
pub fn i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Option::<IntOrString>::deserialize(d)?.map_or(Ok(0), IntOrString::into_i64)
}

/// An optional integer.
pub fn opt_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Option::<IntOrString>::deserialize(d)?
        .map(IntOrString::into_i64)
        .transpose()
}

/// A list of integers; `null` becomes empty.
pub fn vec_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<i64>, D::Error> {
    Option::<Vec<IntOrString>>::deserialize(d)?
        .unwrap_or_default()
        .into_iter()
        .map(IntOrString::into_i64)
        .collect()
}

/// A map of integers; `null` becomes empty.
pub fn map_i64<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, i64>, D::Error> {
    Option::<BTreeMap<String, IntOrString>>::deserialize(d)?
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| v.into_i64::<D::Error>().map(|v| (k, v)))
        .collect()
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FloatOrString {
    Num(f64),
    Str(String),
}

impl FloatOrString {
    fn into_f64<E: serde::de::Error>(self) -> Result<f64, E> {
        match self {
            Self::Num(f) => Ok(f),
            // `f64::from_str` also accepts `NaN`, `inf`, `Infinity`, `-Infinity`.
            Self::Str(s) => s
                .trim()
                .parse()
                .map_err(|_| E::custom(format!("invalid number string {s:?}"))),
        }
    }
}

/// A required float; `null` becomes `0.0`.
pub fn f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Option::<FloatOrString>::deserialize(d)?.map_or(Ok(0.0), FloatOrString::into_f64)
}

/// An optional float.
pub fn opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Option::<FloatOrString>::deserialize(d)?
        .map(FloatOrString::into_f64)
        .transpose()
}

/// A list of floats; `null` becomes empty.
pub fn vec_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
    Option::<Vec<FloatOrString>>::deserialize(d)?
        .unwrap_or_default()
        .into_iter()
        .map(FloatOrString::into_f64)
        .collect()
}

/// A map of floats; `null` becomes empty.
pub fn map_f64<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, f64>, D::Error> {
    Option::<BTreeMap<String, FloatOrString>>::deserialize(d)?
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| v.into_f64::<D::Error>().map(|v| (k, v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq, Default)]
    struct T {
        #[serde(default, deserialize_with = "super::i64")]
        a: i64,
        #[serde(default, deserialize_with = "super::opt_i64")]
        b: Option<i64>,
        #[serde(default, deserialize_with = "super::vec_i64")]
        c: Vec<i64>,
        #[serde(default, deserialize_with = "super::map_i64")]
        d: BTreeMap<String, i64>,
    }

    fn parse(s: &str) -> Result<T, serde_json::Error> {
        serde_json::from_str(s)
    }

    #[test]
    fn numbers_and_numeric_strings() {
        let t =
            parse(r#"{"a":"9007199254740993","b":"-4","c":[1,"2",3.0],"d":{"x":"5"}}"#).unwrap();
        assert_eq!(t.a, 9_007_199_254_740_993);
        assert_eq!(t.b, Some(-4));
        assert_eq!(t.c, vec![1, 2, 3]);
        assert_eq!(t.d["x"], 5);
        let t = parse(r#"{"a":null,"b":null,"c":null,"d":null}"#).unwrap();
        assert_eq!(t, T::default());
        assert_eq!(parse("{}").unwrap(), T::default());
    }

    #[derive(Deserialize, Debug, Default)]
    struct F {
        #[serde(default, deserialize_with = "super::f64")]
        a: f64,
        #[serde(default, deserialize_with = "super::opt_f64")]
        b: Option<f64>,
        #[serde(default, deserialize_with = "super::vec_f64")]
        c: Vec<f64>,
        #[serde(default, deserialize_with = "super::map_f64")]
        d: BTreeMap<String, f64>,
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn floats_accept_non_finite_strings() {
        let f: F =
            serde_json::from_str(r#"{"a":"NaN","b":"-Infinity","c":[1.5,"2.5","Infinity"]}"#)
                .unwrap();
        assert!(f.a.is_nan());
        assert_eq!(f.b, Some(f64::NEG_INFINITY));
        assert_eq!(f.c, vec![1.5, 2.5, f64::INFINITY]);
        let f: F =
            serde_json::from_str(r#"{"a":null,"b":null,"c":null,"d":{"k":"0.25"}}"#).unwrap();
        assert_eq!((f.a, f.b, f.c.len()), (0.0, None, 0));
        assert_eq!(f.d["k"], 0.25);
        assert!(serde_json::from_str::<F>(r#"{"a":"fast"}"#).is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(r#"{"a":"12x"}"#).is_err());
        assert!(parse(r#"{"b":1.5}"#).is_err());
        assert!(parse(r#"{"c":["z"]}"#).is_err());
        assert!(parse(r#"{"d":{"k":"q"}}"#).is_err());
        assert!(parse(r#"{"a":"99999999999999999999"}"#).is_err());
    }
}
