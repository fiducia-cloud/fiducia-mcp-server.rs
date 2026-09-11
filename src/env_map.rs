//! Immutable environment snapshots for flags-2-env consumers.
//!
//! Process environment and argv are copied at the application boundary.
//! CLI overrides merge into an ordinary map. This module never writes
//! `std::env`.

use std::collections::BTreeMap;

/// Deterministic environment snapshot. Prefer this over mutating process env.
pub type EnvMap = BTreeMap<String, String>;

/// Pure merge: later override entries win over the initial map.
pub fn get_env_map(
    initial: EnvMap,
    overrides: impl IntoIterator<Item = (String, String)>,
) -> EnvMap {
    overrides
        .into_iter()
        .fold(initial, |mut env, (key, value)| {
            env.insert(key, value);
            env
        })
}

/// Return a trimmed non-empty value from an environment snapshot.
pub fn env_value<'a>(env: &'a EnvMap, key: &str) -> Option<&'a str> {
    env.get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Copy the process environment. Impure boundary helper.
pub fn process_env_map() -> EnvMap {
    std::env::vars().collect()
}

/// Copy process arguments. Impure boundary helper.
pub fn process_argv() -> Vec<String> {
    std::env::args().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE: &str = "FIDUCIA_NODE_URL";

    #[test]
    fn cli_values_override_environment_values() {
        let initial = EnvMap::from([
            ("PORT".into(), "3000".into()),
            ("HOST".into(), "localhost".into()),
        ]);
        let overrides = EnvMap::from([("PORT".into(), "8080".into())]);
        let env = get_env_map(initial, overrides);

        assert_eq!(env.get("PORT").map(String::as_str), Some("8080"));
        assert_eq!(env.get("HOST").map(String::as_str), Some("localhost"));
    }

    #[test]
    fn empty_override_still_wins() {
        let initial = EnvMap::from([("RUST_LOG".into(), "info".into())]);
        let env = get_env_map(initial, [("RUST_LOG".into(), String::new())]);
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some(""));
        assert_eq!(env_value(&env, "RUST_LOG"), None);
    }

    #[test]
    fn env_value_ignores_empty_and_whitespace_only_entries() {
        for raw in ["", " ", "\t"] {
            let env = EnvMap::from([(PROBE.into(), raw.into())]);
            assert_eq!(env_value(&env, PROBE), None, "raw={raw:?}");
        }
        let env = EnvMap::from([(PROBE.into(), "  /tmp/value  ".into())]);
        assert_eq!(env_value(&env, PROBE), Some("/tmp/value"));
    }

    #[test]
    fn merge_does_not_mutate_process_environment() {
        let before = std::env::var_os(PROBE);
        let env = get_env_map(
            EnvMap::from([(PROBE.into(), "base".into())]),
            [(PROBE.into(), "override".into())],
        );
        assert_eq!(env.get(PROBE).map(String::as_str), Some("override"));
        assert_eq!(std::env::var_os(PROBE), before);
    }

    #[test]
    fn source_does_not_write_process_environment() {
        const SRC: &str = include_str!("env_map.rs");
        let production = SRC.split("#[cfg(test)]").next().unwrap_or(SRC);
        assert!(!production.contains("set_var"));
    }
}
