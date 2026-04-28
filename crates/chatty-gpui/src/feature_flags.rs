pub fn subtle_guidance_v1_enabled() -> bool {
    cfg!(feature = "subtle_guidance_v1")
        || std::env::var("CHATTY_SUBTLE_GUIDANCE_V1")
            .map(|value| env_flag_enabled(&value))
            .unwrap_or(false)
}

fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::env_flag_enabled;

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
}
