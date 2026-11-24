//! Go's `encoding/json` prints a whole-valued float64 without a fraction (`0`, `120`), serde_json adds
//! `.0`. Use `#[serde(serialize_with = "crate::gofloat::serialize")]` on `f64` fields of API payloads.

use serde::Serializer;

pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

#[cfg(test)]
mod tests {
    #[derive(serde::Serialize)]
    struct T {
        #[serde(serialize_with = "super::serialize")]
        v: f64,
    }

    #[test]
    fn whole_floats_print_like_go() {
        for (v, want) in [
            (0.0, "{\"v\":0}"),
            (120.0, "{\"v\":120}"),
            (-3.0, "{\"v\":-3}"),
            (1.5, "{\"v\":1.5}"),
            (0.25, "{\"v\":0.25}"),
        ] {
            assert_eq!(serde_json::to_string(&T { v }).unwrap(), want);
        }
    }
}
