//! Strict, stdio-safe flags2env startup configuration.

use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
};

use flags2env::BundledFlags2Env;
use tracing_subscriber::EnvFilter;

use crate::env_map::{EnvMap, env_value, get_env_map, process_argv, process_env_map};

const DEFAULT_LOG_FILTER: &str = "info,hyper=warn";

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub fn parse_cli_flags(argv: &[String], config_path: &Path) -> Result<EnvMap, Box<dyn Error>> {
    let config_path = config_path
        .to_str()
        .ok_or_else(|| invalid_input(".cli-flags.toml path is not valid UTF-8"))?;
    let parser = BundledFlags2Env::new();
    parser.audit_config(Some(config_path)).map_err(|error| {
        invalid_input(format!("flags-2-env configuration audit failed: {error}"))
    })?;
    let parsed = parser
        .parse_structured(argv, Some(config_path))
        .map_err(|error| invalid_input(format!("flags-2-env parse failed: {error}")))?;

    if !parsed.unknown_options.is_empty() {
        return Err(invalid_input(format!(
            "unknown command-line option(s): {}",
            parsed.unknown_options.join(", ")
        ))
        .into());
    }
    if !parsed.errors.is_empty() {
        return Err(invalid_input(format!(
            "invalid command-line value(s): {}",
            parsed.errors.join("; ")
        ))
        .into());
    }
    if !parsed.extras.is_empty() {
        return Err(invalid_input(format!(
            "unexpected positional argument(s): {}",
            parsed.extras.join(", ")
        ))
        .into());
    }

    let env = get_env_map(EnvMap::new(), parsed.flags);
    let filter = env_value(&env, "RUST_LOG").unwrap_or(DEFAULT_LOG_FILTER);
    EnvFilter::try_new(filter)
        .map_err(|error| invalid_input(format!("invalid --log-filter value: {error}")))?;
    Ok(env)
}

pub fn resolve_config_path() -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = std::env::var_os("FIDUCIA_FLAGS_CONFIG").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(invalid_input("FIDUCIA_FLAGS_CONFIG does not point to a readable file").into());
    }

    let mut candidates = Vec::new();
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join(".cli-flags.toml"));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            candidates.push(parent.join(".cli-flags.toml"));
            candidates.push(parent.join("../share/fiducia-mcp-server/.cli-flags.toml"));
        }
    }

    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            invalid_input("cannot locate .cli-flags.toml; set FIDUCIA_FLAGS_CONFIG to its path")
                .into()
        })
}

pub fn apply_cli_flags() -> Result<EnvMap, Box<dyn Error>> {
    let argv = process_argv();
    let config_path = resolve_config_path()?;
    Ok(get_env_map(
        process_env_map(),
        parse_cli_flags(&argv, &config_path)?,
    ))
}

pub fn process_startup_flags() -> Result<EnvMap, Box<dyn Error>> {
    apply_cli_flags()
}

pub fn process_log_filter() -> Result<EnvFilter, Box<dyn Error>> {
    let env = apply_cli_flags()?;
    let filter = env_value(&env, "RUST_LOG").unwrap_or(DEFAULT_LOG_FILTER);
    EnvFilter::try_new(filter)
        .map_err(|error| invalid_input(format!("invalid --log-filter value: {error}")))
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".cli-flags.toml")
    }

    #[test]
    fn accepts_only_the_declared_stderr_log_filter() {
        let argv = vec![
            "fiducia-mcp".to_owned(),
            "--log-filter=debug,hyper=warn".to_owned(),
        ];
        let env = parse_cli_flags(&argv, &config_path()).expect("valid operational flag");
        assert!(
            env_value(&env, "RUST_LOG")
                .unwrap_or_default()
                .contains("debug")
        );
    }

    #[test]
    fn rejects_secret_bearing_flags() {
        let argv = vec![
            "fiducia-mcp".to_owned(),
            "--fiducia-api-key=must-remain-environment-only".to_owned(),
        ];
        let error = parse_cli_flags(&argv, &config_path())
            .expect_err("secret-bearing option must remain unknown")
            .to_string();
        assert!(error.contains("unknown command-line option"));
    }

    #[test]
    fn rejects_upstream_urls_as_flags() {
        let argv = vec![
            "fiducia-mcp".to_owned(),
            "--fiducia-node-url=https://untrusted.invalid".to_owned(),
        ];
        assert!(parse_cli_flags(&argv, &config_path()).is_err());
    }

    #[test]
    fn rejects_invalid_log_filters() {
        let argv = vec!["fiducia-mcp".to_owned(), "--log-filter=[invalid".to_owned()];
        assert!(parse_cli_flags(&argv, &config_path()).is_err());
    }

    #[test]
    fn cli_overrides_merge_into_map_without_mutating_process_env() {
        let before = std::env::var_os("RUST_LOG");
        let parsed = parse_cli_flags(
            &["fiducia-mcp".into(), "--log-filter=debug".into()],
            &config_path(),
        )
        .expect("valid flags");
        let env = get_env_map(EnvMap::from([("RUST_LOG".into(), "info".into())]), parsed);
        assert_eq!(env_value(&env, "RUST_LOG"), Some("debug"));
        assert_eq!(std::env::var_os("RUST_LOG"), before);
    }

    #[test]
    fn parse_failure_does_not_mutate_process_environment() {
        let before = std::env::var_os("RUST_LOG");
        assert!(parse_cli_flags(
            &["fiducia-mcp".into(), "--this-flag-is-not-declared".into()],
            &config_path(),
        )
        .is_err());
        assert_eq!(std::env::var_os("RUST_LOG"), before);
    }

    #[test]
    fn source_does_not_mutate_process_environment() {
        const SRC: &str = include_str!("flags.rs");
        let production = SRC.split("#[cfg(test)]").next().unwrap_or(SRC);
        assert!(!production.contains("std::env::set_var"));
        assert!(!production.contains("env::set_var"));
    }
}
