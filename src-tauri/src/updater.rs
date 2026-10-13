use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::header::ORIGIN;
use axum::http::HeaderMap;
use serde::Serialize;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use uuid::Uuid;

use super::{stop_backend, write_agent_log, AppState};

pub const UPDATE_TOKEN_HEADER: &str = "x-dendrite-update-token";
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
pub const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub fn diagnostic_is_visible(target: &str) -> bool {
    // The updater SDK logs expected endpoint/network failures at ERROR itself.
    // Optional update checks remain quiet; application/security logs retain
    // their normal visibility and signature verification remains enforced.
    !target.starts_with("tauri_plugin_updater")
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Installing,
    Failed,
}

impl UpdatePhase {
    fn is_busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Downloading | Self::Installing)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateSnapshot {
    pub phase: UpdatePhase,
    pub current_version: String,
    pub available_version: Option<String>,
    pub progress_percent: Option<u8>,
    pub message: String,
    pub checked_at: Option<String>,
}

impl Default for UpdateSnapshot {
    fn default() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            available_version: None,
            progress_percent: None,
            message: "Update check pending".to_string(),
            checked_at: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum UpdateCommand {
    Check,
    Install,
}

pub struct UpdateRuntime {
    snapshot: Mutex<UpdateSnapshot>,
    pending: AsyncMutex<Option<Update>>,
    command_tx: mpsc::Sender<UpdateCommand>,
    page_token: String,
}

impl UpdateRuntime {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<UpdateCommand>) {
        let (command_tx, command_rx) = mpsc::channel(2);
        (
            Arc::new(Self {
                snapshot: Mutex::new(UpdateSnapshot::default()),
                pending: AsyncMutex::new(None),
                command_tx,
                page_token: Uuid::new_v4().simple().to_string(),
            }),
            command_rx,
        )
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn page_token(&self) -> &str {
        &self.page_token
    }

    pub fn request_check(&self) -> Result<(), &'static str> {
        {
            let mut snapshot = self
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if snapshot.phase.is_busy() {
                return Err("An update operation is already running");
            }
            snapshot.phase = UpdatePhase::Checking;
            snapshot.progress_percent = None;
            snapshot.message = "Checking for updates…".to_string();
        }
        if self.command_tx.try_send(UpdateCommand::Check).is_err() {
            self.set_failed();
            return Err("The update service is temporarily unavailable");
        }
        Ok(())
    }

    pub fn request_install(&self) -> Result<(), &'static str> {
        {
            let mut snapshot = self
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if snapshot.phase.is_busy() {
                return Err("An update operation is already running");
            }
            if snapshot.phase != UpdatePhase::Available {
                return Err("No verified update is ready to install");
            }
            snapshot.phase = UpdatePhase::Downloading;
            snapshot.progress_percent = Some(0);
            snapshot.message = "Downloading update…".to_string();
        }
        if self.command_tx.try_send(UpdateCommand::Install).is_err() {
            self.restore_available();
            return Err("The update service is temporarily unavailable");
        }
        Ok(())
    }

    pub fn authorize_page_action(&self, headers: &HeaderMap, port: u16) -> bool {
        let token_matches = headers
            .get(UPDATE_TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == self.page_token);
        let expected_origin = format!("http://127.0.0.1:{port}");
        let origin_matches = headers
            .get(ORIGIN)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == expected_origin);
        token_matches && origin_matches
    }

    fn mutate_snapshot(&self, mutate: impl FnOnce(&mut UpdateSnapshot)) {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        mutate(&mut snapshot);
    }

    fn set_failed(&self) {
        self.mutate_snapshot(|snapshot| {
            snapshot.phase = UpdatePhase::Failed;
            snapshot.progress_percent = None;
            snapshot.message.clear();
            snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
        });
    }

    fn restore_available(&self) {
        self.mutate_snapshot(|snapshot| {
            snapshot.phase = UpdatePhase::Available;
            snapshot.progress_percent = None;
            snapshot.message = snapshot
                .available_version
                .as_ref()
                .map(|version| format!("Version {version} is available"))
                .unwrap_or_default();
        });
    }

    fn set_download_progress(&self, downloaded: u64, total: Option<u64>) {
        let percent = total
            .filter(|total| *total > 0)
            .map(|total| ((downloaded.saturating_mul(100) / total).min(100)) as u8);
        self.mutate_snapshot(|snapshot| {
            snapshot.phase = UpdatePhase::Downloading;
            snapshot.progress_percent = percent;
            snapshot.message = percent
                .map(|value| format!("Downloading update · {value}%"))
                .unwrap_or_else(|| "Downloading update…".to_string());
        });
    }

    fn set_installing(&self) {
        self.mutate_snapshot(|snapshot| {
            snapshot.phase = UpdatePhase::Installing;
            snapshot.progress_percent = Some(100);
            snapshot.message = "Installing update…".to_string();
        });
    }
}

pub async fn run_update_schedule(runtime: Arc<UpdateRuntime>) {
    tokio::time::sleep(Duration::from_secs(2)).await;
    loop {
        let _ = runtime.request_check();
        tokio::time::sleep(UPDATE_CHECK_INTERVAL).await;
    }
}

pub async fn run_update_worker<R: Runtime>(
    app: AppHandle<R>,
    app_state: Arc<Mutex<AppState>>,
    runtime: Arc<UpdateRuntime>,
    mut commands: mpsc::Receiver<UpdateCommand>,
) {
    while let Some(command) = commands.recv().await {
        match command {
            UpdateCommand::Check => check_for_update(&app, &app_state, &runtime).await,
            UpdateCommand::Install => install_update(&app, &runtime).await,
        }
    }
}

async fn check_for_update<R: Runtime>(
    app: &AppHandle<R>,
    app_state: &Arc<Mutex<AppState>>,
    runtime: &Arc<UpdateRuntime>,
) {
    let state_for_exit = app_state.clone();
    let app_for_exit = app.clone();
    let updater = match app
        .updater_builder()
        .timeout(UPDATE_CHECK_TIMEOUT)
        .on_before_exit(move || {
            stop_backend(state_for_exit.clone());
            app_for_exit.cleanup_before_exit();
        })
        .build()
    {
        Ok(updater) => updater,
        Err(_) => {
            runtime.set_failed();
            return;
        }
    };

    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            *runtime.pending.lock().await = Some(update);
            runtime.mutate_snapshot(|snapshot| {
                snapshot.phase = UpdatePhase::Available;
                snapshot.available_version = Some(version.clone());
                snapshot.progress_percent = None;
                snapshot.message = format!("Version {version} is available");
                snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
            });
            write_agent_log(&format!("Dendrite update available: {version}"));
        }
        Ok(None) => {
            *runtime.pending.lock().await = None;
            runtime.mutate_snapshot(|snapshot| {
                snapshot.phase = UpdatePhase::Current;
                snapshot.available_version = None;
                snapshot.progress_percent = None;
                snapshot.message = "Latest version".to_string();
                snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
            });
            write_agent_log("Dendrite is on the latest version");
        }
        Err(_) => {
            runtime.set_failed();
        }
    }
}

async fn install_update<R: Runtime>(_app: &AppHandle<R>, runtime: &Arc<UpdateRuntime>) {
    let update = runtime.pending.lock().await.take();
    let Some(update) = update else {
        runtime.set_failed();
        return;
    };

    let progress_runtime = runtime.clone();
    let finish_runtime = runtime.clone();
    let mut downloaded = 0u64;
    let result = update
        .download_and_install(
            move |chunk_size, total| {
                downloaded = downloaded.saturating_add(chunk_size as u64);
                progress_runtime.set_download_progress(downloaded, total);
            },
            move || finish_runtime.set_installing(),
        )
        .await;

    match result {
        Ok(()) => {
            write_agent_log("Verified Dendrite update installed");
            #[cfg(not(target_os = "windows"))]
            _app.restart();
        }
        Err(_) => {
            *runtime.pending.lock().await = Some(update);
            runtime.restore_available();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    #[test]
    fn update_actions_require_the_runtime_token_and_exact_loopback_origin() {
        let (runtime, _commands) = UpdateRuntime::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(UPDATE_TOKEN_HEADER),
            HeaderValue::from_str(runtime.page_token()).unwrap(),
        );
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:1543"));
        assert!(runtime.authorize_page_action(&headers, 1543));

        headers.insert(ORIGIN, HeaderValue::from_static("https://attacker.example"));
        assert!(!runtime.authorize_page_action(&headers, 1543));
        headers.remove(UPDATE_TOKEN_HEADER);
        assert!(!runtime.authorize_page_action(&headers, 1543));
    }

    #[test]
    fn only_available_updates_can_enter_the_download_state() {
        let (runtime, mut commands) = UpdateRuntime::new();
        assert_eq!(
            runtime.request_install(),
            Err("No verified update is ready to install")
        );
        runtime.mutate_snapshot(|snapshot| {
            snapshot.phase = UpdatePhase::Available;
            snapshot.available_version = Some("9.9.9".to_string());
        });
        assert_eq!(runtime.request_install(), Ok(()));
        assert_eq!(runtime.snapshot().phase, UpdatePhase::Downloading);
        assert!(matches!(commands.try_recv(), Ok(UpdateCommand::Install)));
        assert_eq!(
            runtime.request_check(),
            Err("An update operation is already running")
        );
    }

    #[test]
    fn failed_downloads_remain_retryable() {
        let (runtime, _commands) = UpdateRuntime::new();
        runtime.mutate_snapshot(|snapshot| {
            snapshot.phase = UpdatePhase::Downloading;
            snapshot.available_version = Some("9.9.9".to_string());
        });
        runtime.restore_available();
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.phase, UpdatePhase::Available);
        assert_eq!(snapshot.available_version.as_deref(), Some("9.9.9"));
        assert_eq!(snapshot.message, "Version 9.9.9 is available");
    }

    #[test]
    fn failed_checks_expose_no_user_facing_error_message() {
        let (runtime, _commands) = UpdateRuntime::new();
        runtime.set_failed();
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.phase, UpdatePhase::Failed);
        assert!(snapshot.message.is_empty());
    }

    #[test]
    fn update_checks_repeat_every_six_hours() {
        assert_eq!(UPDATE_CHECK_INTERVAL, Duration::from_secs(21_600));
    }

    #[test]
    fn optional_updater_diagnostics_are_quiet_and_other_logs_remain_visible() {
        assert!(!diagnostic_is_visible("tauri_plugin_updater::updater"));
        assert!(diagnostic_is_visible("app_lib"));
        assert!(diagnostic_is_visible("synthius::security"));
    }
}
