use asterism_provider_api::ExecutionRequest;
use sha2::{Digest, Sha256};

pub(crate) fn uniform_u64(
    domain: &[u8],
    request: &ExecutionRequest,
    minimum: u64,
    maximum: u64,
) -> u64 {
    let digest = execution_digest(domain, request);
    let sample = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"));
    minimum + sample % (maximum - minimum + 1)
}

pub(crate) fn uniform_u8(
    domain: &[u8],
    request: &ExecutionRequest,
    minimum: u8,
    maximum: u8,
) -> u8 {
    let selected = uniform_u64(domain, request, u64::from(minimum), u64::from(maximum));
    u8::try_from(selected).expect("bounded score selection remains a u8")
}

/// Reproduces the current donor's clamped Gaussian score-selection shape from
/// immutable Core-owned execution entropy. Exact donor PRNG output is neither
/// available nor required; the distribution, rounding and bounds are.
pub(crate) fn clamped_gaussian_u8(
    domain: &[u8],
    request: &ExecutionRequest,
    minimum: u8,
    maximum: u8,
) -> u8 {
    if minimum == maximum {
        return minimum;
    }
    let digest = execution_digest(domain, request);
    let first = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"));
    let second = u64::from_be_bytes(digest[8..16].try_into().expect("SHA-256 prefix"));
    // Use the high 53 bits so both uniforms are exactly representable as f64.
    // u1 is strictly inside (0, 1), avoiding ln(0); u2 is in [0, 1).
    let first_53 = first >> 11;
    let second_53 = second >> 11;
    let two_pow_53 = 9_007_199_254_740_992_f64;
    let u1 = (u53_as_f64(first_53) + 1.0) / (two_pow_53 + 1.0);
    let u2 = u53_as_f64(second_53) / two_pow_53;
    let standard_normal = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
    let minimum = f64::from(minimum);
    let maximum = f64::from(maximum);
    let mean = minimum.midpoint(maximum);
    let standard_deviation = (maximum - minimum) / 6.0;
    let selected = (mean + standard_normal * standard_deviation)
        .round_ties_even()
        .clamp(minimum, maximum);
    (0_u8..=100)
        .find(|candidate| (f64::from(*candidate) - selected).abs() < f64::EPSILON)
        .expect("rounded score remains an integer in the configured u8 range")
}

fn u53_as_f64(value: u64) -> f64 {
    debug_assert!(value < (1_u64 << 53));
    let high = u32::try_from(value >> 32).expect("u53 high component fits u32");
    let low = u32::try_from(value & u64::from(u32::MAX)).expect("masked low component fits u32");
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

fn execution_digest(domain: &[u8], request: &ExecutionRequest) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(b"\0");
    hash.update(request.execution_id.to_string().as_bytes());
    hash.update(b"\0");
    hash.update(request.task_id.to_string().as_bytes());
    hash.update(b"\0");
    hash.update(request.remote_task_id.as_bytes());
    hash.finalize().into()
}
