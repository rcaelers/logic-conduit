/// Returns the least-significant-bit-first value at `bit_index`, or `false`
/// when the requested bit lies beyond `data`.
pub(crate) fn get_packed_bit(data: &[u8], bit_index: usize) -> bool {
    data.get(bit_index / 8)
        .is_some_and(|byte| byte & (1 << (bit_index % 8)) != 0)
}

/// Parses the human-readable sample rates used by capture-file metadata.
pub(crate) fn parse_sample_rate(rate: &str) -> Option<f64> {
    let mut parts = rate.split_whitespace();
    let value: f64 = parts.next()?.parse().ok()?;
    let multiplier = match parts.next()? {
        "GHz" => 1e9,
        "MHz" => 1e6,
        "KHz" | "kHz" => 1e3,
        "Hz" => 1.0,
        _ => return None,
    };
    Some(value * multiplier)
}
