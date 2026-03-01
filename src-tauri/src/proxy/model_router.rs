//! Model alias resolution and per-model routing rules.

use crate::models::{ModelAlias, ModelRoute};

pub struct ModelRouter {
    aliases: Vec<ModelAlias>,
    routes: Vec<ModelRoute>,
}

impl ModelRouter {
    pub fn new(aliases: Vec<ModelAlias>, routes: Vec<ModelRoute>) -> Self {
        Self { aliases, routes }
    }

    /// Resolve a model name through alias rules.
    /// Returns the target name if an alias matches, otherwise returns the original name.
    pub fn resolve_alias(&self, model: &str) -> String {
        for alias in &self.aliases {
            if glob_match(&alias.pattern, model) {
                return alias.target.clone();
            }
        }
        model.to_string()
    }

    /// Get preferred account IDs for a model, or None if no route matches.
    /// Routes are sorted by priority (lower = higher priority).
    pub fn preferred_accounts(&self, model: &str) -> Option<Vec<String>> {
        let mut matched: Vec<&ModelRoute> = self
            .routes
            .iter()
            .filter(|r| glob_match(&r.model_pattern, model))
            .collect();

        if matched.is_empty() {
            return None;
        }

        matched.sort_by_key(|r| r.priority);
        // Merge account IDs from all matching rules (priority order).
        let mut ids = Vec::new();
        for route in matched {
            for id in &route.account_ids {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
        Some(ids)
    }
}

/// Simple glob matching: `*` matches any substring.
/// Supports prefix (`*suffix`), suffix (`prefix*`), and contains (`*mid*`).
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        match text[pos..].find(part) {
            Some(idx) => {
                // First part must be a prefix
                if i == 0 && idx != 0 {
                    return false;
                }
                pos += idx + part.len();
            }
            None => return false,
        }
    }

    // Last part must be a suffix
    if let Some(last) = parts.last() {
        if !last.is_empty() && !text.ends_with(last) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(glob_match("gpt-4o", "gpt-4o"));
        assert!(!glob_match("gpt-4o", "gpt-4o-mini"));
    }

    #[test]
    fn wildcard_suffix() {
        assert!(glob_match("gpt-*", "gpt-4o"));
        assert!(glob_match("gpt-*", "gpt-4o-mini"));
        assert!(!glob_match("gpt-*", "claude-3"));
    }

    #[test]
    fn wildcard_prefix() {
        assert!(glob_match("*-sonnet", "claude-3-5-sonnet"));
        assert!(!glob_match("*-sonnet", "claude-3-5-opus"));
    }

    #[test]
    fn wildcard_contains() {
        assert!(glob_match("*claude*", "claude-3-5-sonnet"));
        assert!(glob_match("*claude*", "anthropic.claude-3"));
        assert!(!glob_match("*claude*", "gpt-4o"));
    }

    #[test]
    fn wildcard_all() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn resolve_alias_exact() {
        let router = ModelRouter::new(
            vec![ModelAlias {
                pattern: "claude-3.5-sonnet".to_string(),
                target: "claude-3-5-sonnet-20241022".to_string(),
            }],
            vec![],
        );
        assert_eq!(
            router.resolve_alias("claude-3.5-sonnet"),
            "claude-3-5-sonnet-20241022"
        );
        assert_eq!(router.resolve_alias("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn preferred_accounts_basic() {
        let router = ModelRouter::new(
            vec![],
            vec![ModelRoute {
                model_pattern: "gpt-*".to_string(),
                account_ids: vec!["acc-A".to_string(), "acc-B".to_string()],
                priority: 0,
            }],
        );
        assert_eq!(
            router.preferred_accounts("gpt-4o"),
            Some(vec!["acc-A".to_string(), "acc-B".to_string()])
        );
        assert_eq!(router.preferred_accounts("claude-3"), None);
    }
}
