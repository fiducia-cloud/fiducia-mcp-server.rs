//! Strict, stdio-safe flags2env startup configuration.

use std::{
    io,
    path::{Path, PathBuf},
};

use flags2env::BundledFlags2Env;
use tracing_subscriber::EnvFilter;

use crate::env_map::{env_value, get_env_map, process_argv, process_env_map, EnvMap};

const DEFAULT_LOG_FILTER: &str = "info,hyper=warn";

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub fn parse_cli_flags(argv: &[String], config_path: &Path) -> io::Result<EnvMap> {
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
        )));
    }
    if !parsed.errors.is_empty() {
        return Err(invalid_input(format!(
            "invalid command-line value(s): {}",
            parsed.errors.join("; ")
        )));
    }
    if !parsed.extras.is_empty() {
        return Err(invalid_input(format!(
            "unexpected positional argument(s): {}",
            parsed.extras.join(", ")
        )));
    }

    let env = get_env_map(EnvMap::new(), parsed.flags);
    let filter = env_value(&env, "RUST_LOG").unwrap_or(DEFAULT_LOG_FILTER);
    EnvFilter::try_new(filter)
        .map_err(|error| invalid_input(format!("invalid --log-filter value: {error}")))?;
    Ok(env)
}

pub fn resolve_config_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("FIDUCIA_FLAGS_CONFIG").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(invalid_input(
            "FIDUCIA_FLAGS_CONFIG does not point to a readable file",
        ));
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
        })
}

pub fn apply_cli_flags() -> io::Result<EnvMap> {
    let argv = process_argv();
    let config_path = resolve_config_path()?;
    Ok(get_env_map(
        process_env_map(),
        parse_cli_flags(&argv, &config_path)?,
    ))
}

pub fn process_startup_flags() -> io::Result<EnvMap> {
    apply_cli_flags()
}

pub fn process_log_filter() -> io::Result<EnvFilter> {
    let env = apply_cli_flags()?;
    let filter = env_value(&env, "RUST_LOG").unwrap_or(DEFAULT_LOG_FILTER);
    EnvFilter::try_new(filter)
        .map_err(|error| invalid_input(format!("invalid --log-filter value: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".cli-flags.toml")
    }

    #[test]
    fn accepts_only_the_declared_stderr_log_filter() -> io::Result<()> {
        let argv = vec![
            "fiducia-mcp".to_owned(),
            "--log-filter=debug,hyper=warn".to_owned(),
        ];
        let env = parse_cli_flags(&argv, &config_path())?;
        assert!(env_value(&env, "RUST_LOG")
            .unwrap_or_default()
            .contains("debug"));
        Ok(())
    }

    #[test]
    fn rejects_secret_bearing_flags() -> io::Result<()> {
        let argv = vec![
            "fiducia-mcp".to_owned(),
            "--fiducia-api-key=must-remain-environment-only".to_owned(),
        ];
        let error = match parse_cli_flags(&argv, &config_path()) {
            Err(error) => error,
            Ok(_) => return Err(invalid_input("secret-bearing option was accepted")),
        };
        assert!(error.to_string().contains("unknown command-line option"));
        Ok(())
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
    fn cli_overrides_merge_into_map_without_mutating_process_env() -> io::Result<()> {
        let before = std::env::var_os("RUST_LOG");
        let parsed = parse_cli_flags(
            &["fiducia-mcp".into(), "--log-filter=debug".into()],
            &config_path(),
        )?;
        let env = get_env_map(EnvMap::from([("RUST_LOG".into(), "info".into())]), parsed);
        assert_eq!(env_value(&env, "RUST_LOG"), Some("debug"));
        assert_eq!(std::env::var_os("RUST_LOG"), before);
        Ok(())
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
