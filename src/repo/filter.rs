use crate::repo::FileEntry;

/// Check if a file entry should be excluded based on path patterns.
pub fn should_exclude(entry: &FileEntry) -> bool {
    entry.is_binary || entry.is_generated
}

/// Check if a path is likely a documentation or config file (not source code).
pub fn is_doc_or_config(language: &str) -> bool {
    matches!(language, "Documentation" | "Config" | "Other")
}

/// Classify the risk level based on file characteristics.
pub fn file_risk_level(loc: usize, language: &str) -> &'static str {
    if is_doc_or_config(language) {
        return "low";
    }
    if loc > 1000 {
        "high"
    } else if loc > 500 {
        "medium"
    } else {
        "low"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(path: &str, language: &str, loc: usize, is_binary: bool, is_generated: bool) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: language.to_string(),
            loc,
            is_binary,
            is_generated,
        }
    }

    #[test]
    fn test_should_exclude_binary() {
        let entry = make_entry("image.png", "Other", 0, true, false);
        assert!(should_exclude(&entry));
    }

    #[test]
    fn test_should_exclude_generated() {
        let entry = make_entry("lock.json", "Config", 0, false, true);
        assert!(should_exclude(&entry));
    }

    #[test]
    fn test_should_not_exclude_normal() {
        let entry = make_entry("main.rs", "Rust", 100, false, false);
        assert!(!should_exclude(&entry));
    }

    #[test]
    fn test_is_doc_or_config_doc() {
        assert!(is_doc_or_config("Documentation"));
        assert!(is_doc_or_config("Config"));
        assert!(is_doc_or_config("Other"));
        assert!(!is_doc_or_config("Rust"));
        assert!(!is_doc_or_config("Python"));
    }

    #[test]
    fn test_file_risk_level_high() {
        assert_eq!(file_risk_level(1500, "Rust"), "high");
    }

    #[test]
    fn test_file_risk_level_medium() {
        assert_eq!(file_risk_level(750, "Rust"), "medium");
    }

    #[test]
    fn test_file_risk_level_low() {
        assert_eq!(file_risk_level(100, "Rust"), "low");
        assert_eq!(file_risk_level(100, "Documentation"), "low");
    }
}
