//! Battery telemetry.
//!
//! Nothing on the device reported power before this, so every claim about what
//! drains the Pin was inference. Poll [`get_battery`] around a change and the
//! answer stops being a guess.
//!
//! `dumpsys battery` is the portable source; the instantaneous draw that makes
//! A/B measurement possible lives in sysfs, and not every kernel exposes every
//! file, so each reading is optional rather than an error.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use serde::Serialize;
use tokio::process::Command;
use tokio::sync::RwLock;

const SYSFS: &str = "/sys/class/power_supply/battery";

/// How often the sampler records a reading.
const SAMPLE_INTERVAL_SECS: u64 = 60;
/// ~24h at one sample a minute. In memory only: a redeploy restarts the
/// server, so finish a measurement before shipping the change it measures.
const MAX_SAMPLES: usize = 1440;

static SAMPLES: OnceLock<RwLock<VecDeque<Sample>>> = OnceLock::new();

fn samples() -> &'static RwLock<VecDeque<Sample>> {
    SAMPLES.get_or_init(|| RwLock::new(VecDeque::with_capacity(MAX_SAMPLES)))
}

#[derive(Clone, Copy, Serialize)]
pub struct Sample {
    pub at_ms: u64,
    pub level: i64,
    pub temperature_c: Option<f64>,
    pub voltage_mv: Option<i64>,
}

/// Record a reading a minute so drain can be measured as %/hour.
///
/// The kernel denies the sysfs files that report instantaneous current, so
/// level-over-time is the one power measurement available without root. It is
/// coarse but honest, and it is enough to A/B a change: baseline, change one
/// thing, compare under the same conditions.
pub fn spawn_sampler() {
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(SAMPLE_INTERVAL_SECS));
        loop {
            ticker.tick().await;
            let reading = read_battery().await;
            let Some(level) = reading.level else { continue };
            let sample = Sample {
                at_ms: now_ms(),
                level,
                temperature_c: reading.temperature_c,
                voltage_mv: reading.voltage_mv,
            };
            let mut store = samples().write().await;
            if store.len() == MAX_SAMPLES {
                store.pop_front();
            }
            store.push_back(sample);
        }
    });
    tracing::info!(
        interval_secs = SAMPLE_INTERVAL_SECS,
        "battery sampler started"
    );
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default, Serialize)]
pub struct BatteryInfo {
    /// Charge percentage, 0-100.
    pub level: Option<i64>,
    /// "charging", "discharging", "full", "not_charging", "unknown".
    pub status: Option<String>,
    /// What it is plugged into, if anything: "ac", "usb", "wireless".
    pub plugged: Option<String>,
    pub health: Option<String>,
    /// Degrees Celsius.
    pub temperature_c: Option<f64>,
    pub voltage_mv: Option<i64>,
    /// Instantaneous current. NEGATIVE while discharging on most kernels.
    pub current_ma: Option<f64>,
    /// Remaining charge, from the fuel gauge.
    pub charge_counter_mah: Option<f64>,
    /// Power draw right now. The number to watch across a change: measure a
    /// quiet baseline, make the change, measure again under the same conditions.
    pub power_mw: Option<f64>,
    /// Discharge rate from the sampler, negative while draining. Needs a few
    /// minutes of samples before it means anything.
    pub drain_pct_per_hour: Option<f64>,
    /// How much history that rate is based on.
    pub sampled_minutes: Option<f64>,
    /// Anything that could not be read, so a thin reading is never mistaken
    /// for a healthy one.
    pub unavailable: Vec<String>,
}

pub async fn get_battery() -> Json<BatteryInfo> {
    let mut info = read_battery().await;

    let store = samples().read().await;
    if let (Some(first), Some(last)) = (store.front(), store.back()) {
        let minutes = (last.at_ms.saturating_sub(first.at_ms)) as f64 / 60_000.0;
        if minutes >= 2.0 {
            info.drain_pct_per_hour = Some((last.level - first.level) as f64 / (minutes / 60.0));
            info.sampled_minutes = Some(minutes);
        }
    }

    Json(info)
}

/// The history behind `drain_pct_per_hour`, oldest first.
pub async fn get_battery_history() -> Json<Vec<Sample>> {
    Json(samples().read().await.iter().copied().collect())
}

async fn read_battery() -> BatteryInfo {
    let mut info = BatteryInfo::default();

    match run("/system/bin/dumpsys", &["battery"]).await {
        Ok(output) => parse_dumpsys(&output, &mut info),
        Err(error) => info.unavailable.push(format!("dumpsys battery: {error}")),
    }

    // µA on virtually every kernel, and sign conventions vary by vendor — the
    // magnitude is what matters for comparison.
    if let Some(value) = read_sysfs_i64("current_now", &mut info).await {
        info.current_ma = Some(value as f64 / 1000.0);
    }
    if let Some(value) = read_sysfs_i64("charge_counter", &mut info).await {
        info.charge_counter_mah = Some(value as f64 / 1000.0);
    }

    if let (Some(current_ma), Some(voltage_mv)) = (info.current_ma, info.voltage_mv) {
        // mA * mV = µW; report milliwatts, unsigned, as a drain magnitude.
        info.power_mw = Some((current_ma * voltage_mv as f64 / 1000.0).abs());
    }

    info
}

// ── Per-component attribution ───────────────────────────────────────

#[derive(Serialize)]
pub struct PowerConsumer {
    /// As `dumpsys` names it, e.g. "UID u0a33" or "Screen".
    pub name: String,
    /// Package(s) behind the uid. "UID u0a33" means nothing on its own.
    pub packages: Vec<String>,
    pub mah: f64,
    /// Share of the attributed drain, so the ranking reads at a glance.
    pub pct_of_drain: Option<f64>,
}

#[derive(Default, Serialize)]
pub struct BatteryStats {
    /// Battery capacity as the framework understands it.
    pub capacity_mah: Option<f64>,
    /// What the framework attributes to everything it tracked.
    pub computed_drain_mah: Option<f64>,
    /// Biggest consumers first. This is the attribution that says whether the
    /// laser, the radio or something else is actually costing you the charge.
    pub consumers: Vec<PowerConsumer>,
    /// What the listed consumers add up to. Well short of `computed_drain_mah`
    /// means the framework is attributing drain we are not seeing.
    pub attributed_mah: Option<f64>,
    pub unavailable: Vec<String>,
}

pub async fn get_battery_stats() -> Json<BatteryStats> {
    let mut stats = BatteryStats::default();

    let packages = match run("/system/bin/cmd", &["package", "list", "packages", "-U"]).await {
        Ok(output) => parse_package_uids(&output),
        Err(error) => {
            stats.unavailable.push(format!("package list: {error}"));
            std::collections::HashMap::new()
        }
    };

    match run("/system/bin/dumpsys", &["batterystats"]).await {
        Ok(output) => parse_batterystats(&output, &mut stats),
        Err(error) => stats
            .unavailable
            .push(format!("dumpsys batterystats: {error}")),
    }

    for consumer in &mut stats.consumers {
        if let Some(uid) = uid_from_label(&consumer.name) {
            if let Some(names) = packages.get(&uid) {
                consumer.packages = names.clone();
            } else if let Some(known) = well_known_uid(uid) {
                consumer.packages = vec![known.to_string()];
            }
        }
    }

    let attributed: f64 = stats.consumers.iter().map(|c| c.mah).sum();
    stats.attributed_mah = Some(attributed);
    if attributed > 0.0 {
        for consumer in &mut stats.consumers {
            consumer.pct_of_drain = Some(consumer.mah / attributed * 100.0);
        }
    }

    if stats.consumers.is_empty() && stats.unavailable.is_empty() {
        stats.unavailable.push(
            "no 'Estimated power use' section — the framework may not have accumulated stats yet"
                .to_string(),
        );
    }

    Json(stats)
}

/// Pull the "Estimated power use (mAh)" block out of `dumpsys batterystats`.
///
/// The surrounding dump is enormous, so this reads only that section. It runs
/// to the next section header rather than the first blank line: blank lines
/// appear *inside* the block, and stopping at one drops the hardware rows
/// (Screen, Wifi, Cell standby, Idle) that sit below the per-uid rows — which
/// is exactly where a laser projector's cost would show up.
fn parse_batterystats(output: &str, stats: &mut BatteryStats) {
    let mut in_section = false;

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Estimated power use (mAh)") {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        // A new top-level section: unindented and not one of our rows.
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }

        if let Some(rest) = trimmed.strip_prefix("Capacity:") {
            for part in rest.split(',') {
                let part = part.trim();
                if let Some(value) = part.strip_prefix("Computed drain:") {
                    stats.computed_drain_mah = value.trim().parse().ok();
                } else if stats.capacity_mah.is_none() {
                    stats.capacity_mah = part.parse().ok();
                }
            }
            continue;
        }

        // "Uid u0a123: 12.3 ( cpu=10 wifi=2 )" or "Screen: 45.6"
        let Some((name, rest)) = trimmed.split_once(':') else {
            continue;
        };
        let value = rest.split('(').next().unwrap_or("").trim();
        if let Ok(mah) = value.parse::<f64>() {
            stats.consumers.push(PowerConsumer {
                name: name.trim().to_string(),
                packages: Vec::new(),
                mah,
                pct_of_drain: None,
            });
        }
    }

    stats
        .consumers
        .sort_by(|a, b| b.mah.partial_cmp(&a.mah).unwrap_or(std::cmp::Ordering::Equal));
    stats.consumers.truncate(40);
}

fn parse_dumpsys(output: &str, info: &mut BatteryInfo) {
    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "level" => info.level = value.parse().ok(),
            "voltage" => info.voltage_mv = value.parse().ok(),
            // Reported in tenths of a degree.
            "temperature" => info.temperature_c = value.parse::<f64>().ok().map(|t| t / 10.0),
            "status" => info.status = Some(battery_status(value)),
            "health" => info.health = Some(battery_health(value)),
            "AC powered" if value == "true" => info.plugged = Some("ac".into()),
            "USB powered" if value == "true" => info.plugged = Some("usb".into()),
            "Wireless powered" if value == "true" => info.plugged = Some("wireless".into()),
            _ => {}
        }
    }
}

/// android.os.BatteryManager BATTERY_STATUS_*
fn battery_status(value: &str) -> String {
    match value {
        "2" => "charging",
        "3" => "discharging",
        "4" => "not_charging",
        "5" => "full",
        _ => "unknown",
    }
    .to_string()
}

/// android.os.BatteryManager BATTERY_HEALTH_*
fn battery_health(value: &str) -> String {
    match value {
        "2" => "good",
        "3" => "overheat",
        "4" => "dead",
        "5" => "over_voltage",
        "6" => "unspecified_failure",
        "7" => "cold",
        _ => "unknown",
    }
    .to_string()
}

async fn read_sysfs_i64(name: &str, info: &mut BatteryInfo) -> Option<i64> {
    let path = format!("{SYSFS}/{name}");
    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => match contents.trim().parse::<i64>() {
            Ok(value) => Some(value),
            Err(error) => {
                info.unavailable.push(format!("{name}: unparseable ({error})"));
                None
            }
        },
        Err(error) => {
            info.unavailable.push(format!("{name}: {error}"));
            None
        }
    }
}

/// `cmd package list packages -U` prints "package:<name> uid:<n>".
fn parse_package_uids(output: &str) -> std::collections::HashMap<i64, Vec<String>> {
    let mut map: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    for line in output.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("package:") else {
            continue;
        };
        let Some((name, uid)) = rest.rsplit_once("uid:") else {
            continue;
        };
        let Ok(uid) = uid.trim().parse::<i64>() else {
            continue;
        };
        map.entry(uid).or_default().push(name.trim().to_string());
    }
    map
}

/// "UID u0a33" is app-id 33 in user 0, which is uid 10033; "UID 1000" is literal.
fn uid_from_label(label: &str) -> Option<i64> {
    let raw = label.strip_prefix("UID")?.trim();
    if let Some(app) = raw.strip_prefix("u0a") {
        return app.parse::<i64>().ok().map(|id| 10_000 + id);
    }
    raw.parse::<i64>().ok()
}

/// Android's reserved uids, which own no package to look up.
fn well_known_uid(uid: i64) -> Option<&'static str> {
    Some(match uid {
        0 => "root",
        1000 => "android (system_server)",
        1001 => "radio / telephony",
        1002 => "bluetooth",
        1013 => "mediaserver",
        1041 => "audioserver",
        1046 => "mediacodec",
        1047 => "cameraserver",
        _ => return None,
    })
}

async fn run(command: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("failed to run {command}: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "{command} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
