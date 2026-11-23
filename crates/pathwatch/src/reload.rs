//! Reloading the configuration file while a session runs.
//!
//! The file is parsed and validated in full first. If anything is wrong, or
//! if it changes a setting that cannot be applied to a running session,
//! nothing at all is changed and the reason is returned. Only then is the
//! new plan handed to the collector.

use std::path::PathBuf;
use std::sync::Arc;

use pathwatch_core::collector;
use pathwatch_core::config::{Config, ConfigError, Loaded, restart_only_changes};
use tokio::sync::mpsc;

pub struct Reloader {
    /// The file named on the command line, if one was.
    explicit: Option<PathBuf>,
    /// What is running now: the next reload is compared against it.
    current: Config,
    tx: mpsc::Sender<collector::Reload>,
}

impl Reloader {
    pub fn new(explicit: Option<PathBuf>, current: Config) -> (Self, mpsc::Receiver<collector::Reload>) {
        let (tx, rx) = mpsc::channel(4);
        (Self { explicit, current, tx }, rx)
    }

    /// Reads the file again and, if it can be applied whole, applies it.
    /// The error says why nothing was changed.
    pub fn reload(&mut self) -> Result<(), String> {
        let opt = crate::ConfigOpt { config: self.explicit.clone() };
        let Some(path) = crate::config_path(&opt) else {
            let hint = crate::default_config_path().map(|p| format!(" Create {} or start with --config.", p.display())).unwrap_or_default();
            return Err(format!("there is no configuration file to reload: this session runs on the built-in configuration.{hint}"));
        };
        let loaded = Loaded::load(Some(&path), &[]).map_err(|e| match e {
            ConfigError::Invalid(problems) => {
                let shown: Vec<String> = problems.iter().take(3).map(ToString::to_string).collect();
                let more = if problems.len() > 3 { format!(" (and {} more; `pathwatch config check` lists them all)", problems.len() - 3) } else { String::new() };
                format!("{} is invalid: {}{more}", path.display(), shown.join("; "))
            }
            other => other.to_string(),
        })?;
        let fixed = restart_only_changes(&self.current, &loaded.config);
        if !fixed.is_empty() {
            return Err(format!("{} changes {}, which a running session cannot take: restart to apply it", path.display(), fixed.join(", ")));
        }
        let plan = crate::plan_for(&loaded);
        self.tx
            .try_send(collector::Reload { config: Arc::new(loaded.config.clone()), plan })
            .map_err(|_| "the collector is still applying an earlier reload; try again".to_string())?;
        self.current = loaded.config;
        Ok(())
    }
}

/// The text of the event recorded when a reload is refused.
pub fn refused(why: &str) -> String {
    format!("configuration not reloaded, and nothing was changed: {why}")
}
