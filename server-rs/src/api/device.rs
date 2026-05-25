use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use tokio::process::Command;
use tracing::error;

use super::ApiState;
use crate::config::LlmProvider;

const HUMANE_DISPLAY_VERSION_SETTING: &str = "penumbra.humane_display_version";

#[derive(Clone, Serialize)]
pub struct ComponentVersion {
    role: &'static str,
    label: &'static str,
    package_name: &'static str,
    version_name: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct OsVersionInfo {
    humane_display_version: Option<String>,
    android_release: Option<String>,
    android_sdk: Option<String>,
    security_patch: Option<String>,
}

impl OsVersionInfo {
    async fn collect() -> Self {
        OsVersionInfo {
            humane_display_version: get_global_setting(HUMANE_DISPLAY_VERSION_SETTING).await,
            android_release: getprop("ro.build.version.release").await,
            android_sdk: getprop("ro.build.version.sdk").await,
            security_patch: getprop("ro.build.version.security_patch").await,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct DeviceVersionSnapshot {
    captured_at_ms: u128,
    runtime_server_version: &'static str,
    components: Vec<ComponentVersion>,
    os: OsVersionInfo,
}

#[derive(Clone, Copy)]
struct ManagedComponent {
    role: &'static str,
    label: &'static str,
    package_name: &'static str,
}

pub struct DeviceVersionCollector;

impl DeviceVersionCollector {
    const MANAGED_COMPONENTS: &'static [ManagedComponent] = &[
        ManagedComponent {
            role: "installer",
            label: "System Injector",
            package_name: "com.penumbraos.systeminjector",
        },
        ManagedComponent {
            role: "hook",
            label: "Hook",
            package_name: "com.penumbraos.hook",
        },
        ManagedComponent {
            role: "server",
            label: "Server",
            package_name: "com.penumbraos.server",
        },
        ManagedComponent {
            role: "injector",
            label: "Hook Injector",
            package_name: "com.penumbraos.hook.injector",
        },
    ];

    pub async fn collect() -> DeviceVersionSnapshot {
        let captured_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();

        let mut components = Vec::with_capacity(Self::MANAGED_COMPONENTS.len());
        for component in Self::MANAGED_COMPONENTS {
            if let Some(version) = Self::query_component_version(component).await {
                components.push(version);
            }
        }

        DeviceVersionSnapshot {
            captured_at_ms,
            runtime_server_version: env!("PENUMBRA_VERSION"),
            components,
            os: OsVersionInfo::collect().await,
        }
    }

    async fn query_component_version(component: &ManagedComponent) -> Option<ComponentVersion> {
        #[cfg(not(target_os = "android"))]
        {
            return Some(ComponentVersion {
                role: component.role,
                label: component.label,
                package_name: component.package_name,
                version_name: None,
                error: Some("package metadata is only available on Android".to_string()),
            });
        }

        #[cfg(target_os = "android")]
        {
            let installed = run_command(
                "/system/bin/pm",
                &["list", "packages", component.package_name],
            )
            .await
            .map(|output| {
                output
                    .lines()
                    .any(|line| line.trim() == format!("package:{}", component.package_name))
            })
            .unwrap_or(false);

            if !installed {
                return None;
            }

            match run_command("/system/bin/dumpsys", &["package", component.package_name]).await {
                Ok(output) => Some(ComponentVersion {
                    role: component.role,
                    label: component.label,
                    package_name: component.package_name,
                    version_name: parse_dumpsys_field(&output, "versionName"),
                    error: None,
                }),
                Err(error) => Some(ComponentVersion {
                    role: component.role,
                    label: component.label,
                    package_name: component.package_name,
                    version_name: None,
                    error: Some(error),
                }),
            }
        }
    }
}

#[derive(Serialize)]
pub struct DeviceInfo {
    display_name: String,
    http_bind_addr: String,
    grpc_bind_addr: String,
    llm_provider: LlmProvider,
    llm_model: String,
    versions: DeviceVersionSnapshot,
}

pub struct DeviceApi;

impl DeviceApi {
    pub async fn get_device(State(state): State<ApiState>) -> Json<DeviceInfo> {
        let (display_name, http_bind_addr, grpc_bind_addr, llm_provider, llm_model) = {
            let config = state.shared_config.read().await;
            (
                config
                    .server
                    .display_name
                    .clone()
                    .unwrap_or_else(|| "Penumbra".into()),
                config.server.http_bind_addr.clone(),
                config.server.grpc_bind_addr.clone(),
                config.llm.provider,
                config.llm.model.clone(),
            )
        };

        let mut versions = state.device_versions.clone();
        versions.os.humane_display_version =
            get_global_setting(HUMANE_DISPLAY_VERSION_SETTING).await;

        Json(DeviceInfo {
            display_name,
            http_bind_addr,
            grpc_bind_addr,
            llm_provider,
            llm_model,
            versions,
        })
    }
}

#[cfg(target_os = "android")]
fn parse_dumpsys_field(output: &str, field: &str) -> Option<String> {
    output.split_whitespace().find_map(|token| {
        token
            .strip_prefix(field)
            .and_then(|value| value.strip_prefix('='))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

#[allow(unused_variables)]
async fn getprop(name: &str) -> Option<String> {
    #[cfg(not(target_os = "android"))]
    {
        return None;
    }

    #[allow(unused)]
    match run_command("/system/bin/getprop", &[name]).await {
        Ok(result) => non_empty_value(&result),
        Err(e) => {
            error!("getprop {name} failed: {e}");
            None
        }
    }
}

#[allow(unused_variables)]
async fn get_global_setting(name: &str) -> Option<String> {
    #[cfg(not(target_os = "android"))]
    {
        return None;
    }

    #[allow(unused)]
    match run_command("/system/bin/settings", &["get", "global", name]).await {
        Ok(result) => non_empty_value(&result),
        Err(e) => {
            error!("settings get global {name} failed: {e}");
            None
        }
    }
}

fn non_empty_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "null" {
        None
    } else {
        Some(trimmed.to_string())
    }
}

async fn run_command(command: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("failed to run {command}: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!(
            "{command} exited with {}: {message}",
            output.status
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
