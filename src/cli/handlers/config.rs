//! Config-related CLI handlers: the `serve` config-file watcher below, and
//! the `reng config provider` subcommand family in [`provider`].

pub mod provider;
#[cfg(test)]
mod tests;

/// Watch a config file for changes and log a warning when modified.
/// This allows users to restart the app to pick up changes.
pub async fn watch_config_file(path: std::path::PathBuf) {
    tokio::task::spawn_blocking(move || {
        use notify::{EventKind, Watcher};
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("Failed to start config watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&path, notify::RecursiveMode::NonRecursive) {
            tracing::warn!("Failed to watch config file: {}", e);
            return;
        }

        loop {
            match rx.recv() {
                Ok(Ok(event)) => {
                    if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        tracing::warn!(
                            "Config file '{}' has changed. Restart review-engine to apply changes.",
                            path.display()
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!("Config watcher error: {}", e);
                }
                Err(_) => break,
            }
        }
    })
    .await
    .ok();
}
