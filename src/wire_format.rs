use crate::models::BenchmarkStats;

/// Parsed harness output
#[derive(Debug)]
pub struct HarnessOutput {
    pub return_value: i64,
    pub iterations: u64,
    pub benchmark: BenchmarkStats,
    pub user_stdout: String,
}

/// Parse the harness wire format from captured stdout.
///
/// Format:
/// ```text
/// HARNESS:rv=<i64>;n=<u64>;freq=24000000;total=<u64>;mean=<u64>;median=<u64>;min=<u64>;max=<u64>;stddev=<u64>\n
/// <user stdout>
/// ```
pub fn parse_harness_output(stdout: &str) -> Result<HarnessOutput, String> {
    // The harness line must be the first line
    let (harness_line, user_stdout) = match stdout.split_once('\n') {
        Some((first, rest)) => (first, rest.to_string()),
        None => (stdout, String::new()),
    };

    if !harness_line.starts_with("HARNESS:") {
        return Err("Missing HARNESS header line in output".to_string());
    }

    let payload = &harness_line["HARNESS:".len()..];
    let fields = parse_fields(payload)?;

    let rv = get_field_i64(&fields, "rv")?;
    let n = get_field_u64(&fields, "n")?;
    let freq = get_field_u64(&fields, "freq")?;
    let total_ticks = get_field_u64(&fields, "total")?;
    let mean_ticks = get_field_u64(&fields, "mean")?;
    let median_ticks = get_field_u64(&fields, "median")?;
    let min_ticks = get_field_u64(&fields, "min")?;
    let max_ticks = get_field_u64(&fields, "max")?;
    let stddev_ticks = get_field_u64(&fields, "stddev")?;

    if freq == 0 {
        return Err("freq cannot be zero".to_string());
    }

    Ok(HarnessOutput {
        return_value: rv,
        iterations: n,
        benchmark: BenchmarkStats {
            iterations: n,
            total_ns: ticks_to_ns(total_ticks, freq),
            mean_ns: ticks_to_ns(mean_ticks, freq),
            median_ns: ticks_to_ns(median_ticks, freq),
            min_ns: ticks_to_ns(min_ticks, freq),
            max_ns: ticks_to_ns(max_ticks, freq),
            stddev_ns: ticks_to_ns(stddev_ticks, freq),
        },
        user_stdout,
    })
}

fn ticks_to_ns(ticks: u64, freq: u64) -> u64 {
    // ns = ticks * 1_000_000_000 / freq
    // Use u128 to avoid overflow
    ((ticks as u128) * 1_000_000_000 / (freq as u128)) as u64
}

fn parse_fields(payload: &str) -> Result<Vec<(String, String)>, String> {
    let mut fields = Vec::new();
    for part in payload.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("Malformed field in harness output: '{}'", part))?;
        fields.push((key.to_string(), value.to_string()));
    }
    Ok(fields)
}

fn get_field_i64(fields: &[(String, String)], key: &str) -> Result<i64, String> {
    let value = fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("Missing field '{}' in harness output", key))?;
    value
        .parse::<i64>()
        .map_err(|e| format!("Invalid value for '{}': {} ({})", key, value, e))
}

fn get_field_u64(fields: &[(String, String)], key: &str) -> Result<u64, String> {
    let value = fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("Missing field '{}' in harness output", key))?;
    value
        .parse::<u64>()
        .map_err(|e| format!("Invalid value for '{}': {} ({})", key, value, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_output() {
        let stdout = "HARNESS:rv=5050;n=9000;freq=24000000;total=1107000;mean=123;median=121;min=118;max=302;stddev=11\nHello from user code\n";
        let result = parse_harness_output(stdout).unwrap();
        assert_eq!(result.return_value, 5050);
        assert_eq!(result.iterations, 9000);
        assert_eq!(result.user_stdout, "Hello from user code\n");
        assert_eq!(result.benchmark.iterations, 9000);
        // Check tick-to-ns conversion: 123 ticks * 1e9 / 24e6 = 5125 ns
        assert_eq!(result.benchmark.mean_ns, 5125);
    }

    #[test]
    fn test_parse_no_user_stdout() {
        let stdout =
            "HARNESS:rv=42;n=1;freq=24000000;total=100;mean=100;median=100;min=100;max=100;stddev=0\n";
        let result = parse_harness_output(stdout).unwrap();
        assert_eq!(result.return_value, 42);
        assert_eq!(result.user_stdout, "");
    }

    #[test]
    fn test_parse_negative_return_value() {
        let stdout =
            "HARNESS:rv=-1;n=1;freq=24000000;total=100;mean=100;median=100;min=100;max=100;stddev=0\n";
        let result = parse_harness_output(stdout).unwrap();
        assert_eq!(result.return_value, -1);
    }

    #[test]
    fn test_parse_missing_harness_header() {
        let stdout = "Some random output\n";
        assert!(parse_harness_output(stdout).is_err());
    }

    #[test]
    fn test_parse_malformed_field() {
        let stdout = "HARNESS:rv5050;n=1;freq=24000000;total=100;mean=100;median=100;min=100;max=100;stddev=0\n";
        assert!(parse_harness_output(stdout).is_err());
    }

    #[test]
    fn test_parse_missing_field() {
        let stdout = "HARNESS:rv=5050;freq=24000000;total=100;mean=100;median=100;min=100;max=100;stddev=0\n";
        assert!(parse_harness_output(stdout).is_err());
    }

    #[test]
    fn test_parse_invalid_number() {
        let stdout = "HARNESS:rv=abc;n=1;freq=24000000;total=100;mean=100;median=100;min=100;max=100;stddev=0\n";
        assert!(parse_harness_output(stdout).is_err());
    }

    #[test]
    fn test_ticks_to_ns_conversion() {
        // 24 ticks at 24MHz = 1000 ns
        assert_eq!(ticks_to_ns(24, 24_000_000), 1000);
        // 24_000_000 ticks at 24MHz = 1 second = 1e9 ns
        assert_eq!(ticks_to_ns(24_000_000, 24_000_000), 1_000_000_000);
        // 0 ticks = 0 ns
        assert_eq!(ticks_to_ns(0, 24_000_000), 0);
    }

    #[test]
    fn test_parse_multiline_user_stdout() {
        let stdout = "HARNESS:rv=0;n=1;freq=24000000;total=100;mean=100;median=100;min=100;max=100;stddev=0\nline1\nline2\nline3\n";
        let result = parse_harness_output(stdout).unwrap();
        assert_eq!(result.user_stdout, "line1\nline2\nline3\n");
    }

    #[test]
    fn test_parse_empty_stdout() {
        let stdout = "";
        assert!(parse_harness_output(stdout).is_err());
    }
}