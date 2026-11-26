//! What this device calls itself when it signs in to Hive (HX-7): the
//! machine's host name, shown in the person's list of sessions.

/// The host name, trimmed; `None` when it is blank or not valid UTF-8. The
/// server shortens nothing, so a name over its limit (80 characters) is cut
/// here rather than refused there.
pub fn device_name() -> Option<String> {
    let name = gethostname::gethostname().into_string().ok()?;
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_device_name_is_short_and_printable() {
        if let Some(name) = super::device_name() {
            assert!(name.chars().count() <= 80);
            assert!(!name.chars().any(char::is_control));
            assert_eq!(name, name.trim());
        }
    }
}
