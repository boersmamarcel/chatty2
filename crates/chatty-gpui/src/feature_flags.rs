pub fn subtle_guidance_v1_enabled() -> bool {
    cfg!(feature = "subtle_guidance_v1")
        || std::env::var("CHATTY_SUBTLE_GUIDANCE_V1")
            .map(|value| env_flag_enabled(&value))
            .unwrap_or(false)
}

pub fn subtle_guidance_skeleton_enabled(seed: &str) -> bool {
    cfg!(feature = "subtle_guidance_skeleton")
        || std::env::var("CHATTY_SUBTLE_GUIDANCE_SKELETON")
            .map(|value| rollout_flag_enabled(&value, seed))
            .unwrap_or(false)
}

pub fn subtle_guidance_traces_enabled() -> bool {
    cfg!(feature = "subtle_guidance_traces")
        || std::env::var("CHATTY_SUBTLE_GUIDANCE_TRACES")
            .map(|value| env_flag_enabled(&value))
            .unwrap_or(false)
}

pub fn stream_smoothing_v1_enabled(seed: &str) -> bool {
    cfg!(feature = "stream_smoothing_v1")
        || std::env::var("CHATTY_STREAM_SMOOTHING_V1")
            .map(|value| rollout_flag_enabled(&value, seed))
            .unwrap_or(false)
}

fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn rollout_flag_enabled(value: &str, seed: &str) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "ab" | "50" | "50%" | "rollout" => stable_bucket(seed) < 50,
        other => env_flag_enabled(other),
    }
}

fn stable_bucket(seed: &str) -> u64 {
    seed.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        hash.wrapping_mul(0x100000001b3) ^ u64::from(byte)
    }) % 100
}

#[cfg(test)]
mod tests {
    use super::{env_flag_enabled, rollout_flag_enabled, stable_bucket};

    #[test]
    fn env_flag_enabled_accepts_truthy_values() {
        for value in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert!(env_flag_enabled(value));
        }
    }

    #[test]
    fn env_flag_enabled_rejects_other_values() {
        for value in ["", "0", "false", "off", "no", "enabled"] {
            assert!(!env_flag_enabled(value));
        }
    }

    #[test]
    fn rollout_flag_uses_truthy_values_or_stable_bucket() {
        assert!(rollout_flag_enabled("true", "anything"));
        assert!(!rollout_flag_enabled("false", "anything"));
        assert_eq!(
            rollout_flag_enabled("ab", "conversation-a"),
            stable_bucket("conversation-a") < 50
        );
    }
}
