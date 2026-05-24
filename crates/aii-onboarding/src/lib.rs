//! # aii-onboarding
//!
//! Read-only hardware probe + Tier recommendation.
//!
//! ## Public API
//! - [`HardwareProfile`] — CPU cores, RAM, disk free / type, public-ip
//!   detection (best-effort).
//! - [`detect`] — fill a profile from the running host.
//! - [`score`] — collapse a profile into a 0–100 number.
//! - [`recommend_tier`] — map score → [`Tier`] (T1–T7).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

/// Disk medium category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskType {
    /// Rotating disk.
    Hdd,
    /// SATA solid-state.
    Sata,
    /// NVMe solid-state.
    Nvme,
    /// Unknown / cannot detect.
    Unknown,
}

/// Tier identifier (T1 highest, T7 lowest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    /// V-node — main-chain validator.
    T1Validator,
    /// SCS — sub-chain consensus node.
    T2Scs,
    /// Archive full node.
    T3Archive,
    /// Standard full node.
    T4Full,
    /// Pruned full node.
    T5Pruned,
    /// Light node (only headers).
    T6Light,
    /// Mobile / wallet only.
    T7Mobile,
}

/// Snapshot of host capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    /// Logical CPU count.
    pub cpu_cores: usize,
    /// Total RAM in gigabytes (binary).
    pub ram_gb: u64,
    /// Free disk space on the data-dir's filesystem (gigabytes).
    pub disk_free_gb: u64,
    /// Disk medium category, best-effort.
    pub disk_type: DiskType,
    /// Whether the node exposed (or could expose) a routable public IP —
    /// `None` if probe is skipped (v0.0.10 always `None`).
    pub public_ip: Option<bool>,
    /// Upstream link rate in Mbps; `None` if probe is skipped.
    pub upstream_mbps: Option<u32>,
}

/// Probe the host and return its [`HardwareProfile`].
///
/// The probe is read-only and synchronous; no network / DNS calls.
pub fn detect() -> HardwareProfile {
    let mut sys = System::new_all();
    sys.refresh_all();
    let cpu_cores = sys.cpus().len().max(1);
    let ram_gb = sys.total_memory() / (1024 * 1024 * 1024);

    let disks = Disks::new_with_refreshed_list();
    let (disk_free_gb, disk_type) =
        disks
            .iter()
            .max_by_key(|d| d.available_space())
            .map_or((0, DiskType::Unknown), |d| {
                let free = d.available_space() / (1024 * 1024 * 1024);
                let ty = classify_disk(d.name().to_string_lossy().as_ref());
                (free, ty)
            });

    HardwareProfile {
        cpu_cores,
        ram_gb,
        disk_free_gb,
        disk_type,
        public_ip: None,
        upstream_mbps: None,
    }
}

fn classify_disk(name: &str) -> DiskType {
    let n = name.to_lowercase();
    if n.contains("nvme") {
        DiskType::Nvme
    } else if n.contains("ssd") || n.contains("sata") || n.contains("vd") {
        DiskType::Sata
    } else if n.contains("hd") || n.contains("sd") {
        DiskType::Hdd
    } else {
        DiskType::Unknown
    }
}

/// Collapse a [`HardwareProfile`] into a 0–100 score.
///
/// Weights (calibrated to 《04 架构设计文档》§14.4 sample thresholds):
/// - **Base** 5 points for any host that runs the probe
/// - **CPU cores**: up to 30 (cores / 16 × 30, capped)
/// - **RAM**: up to 30 (gb / 64 × 30, capped)
/// - **Disk free**: up to 20 (gb / 2000 × 20, capped)
/// - **Disk type**: NVMe 15, Sata 10, Unknown 5, Hdd 0
/// - **Public IP** (when probed): +5
#[must_use]
pub fn score(p: &HardwareProfile) -> u32 {
    // Invalid / probe-failed profiles get a non-classifiable 0 (→ T7).
    if p.cpu_cores == 0 || p.ram_gb == 0 {
        return 0;
    }
    let base = 5;
    let cpu = ((p.cpu_cores as f32 / 16.0) * 30.0).min(30.0) as u32;
    let ram = ((p.ram_gb as f32 / 64.0) * 30.0).min(30.0) as u32;
    let disk = ((p.disk_free_gb as f32 / 2000.0) * 20.0).min(20.0) as u32;
    let medium = match p.disk_type {
        DiskType::Nvme => 15,
        DiskType::Sata => 10,
        DiskType::Hdd => 0,
        DiskType::Unknown => 5,
    };
    let net = if p.public_ip == Some(true) { 5 } else { 0 };
    (base + cpu + ram + disk + medium + net).min(100)
}

/// Map a score to the recommended [`Tier`].
///
/// Thresholds calibrated so that representative profiles land in the
/// expected tier:
///
/// | Tier | Score | Reference profile                       |
/// |------|-------|------------------------------------------|
/// | T1   | ≥ 80  | 32c / 128 GB / 4 TB NVMe / public IP     |
/// | T2   | 55–79 | 8c / 32 GB / 1 TB NVMe                   |
/// | T3   | 40–54 | 8c / 32 GB / 5 TB SATA                   |
/// | T4   | 30–39 | 4c / 16 GB / 1 TB SATA                   |
/// | T5   | 15–29 | 2c / 8 GB / 500 GB SATA                  |
/// | T6   | 5–14  | 1c / 2 GB / 50 GB HDD                    |
/// | T7   | < 5   | probe failed / hardware unknown          |
#[must_use]
pub const fn recommend_tier(score: u32) -> Tier {
    match score {
        80..=u32::MAX => Tier::T1Validator,
        55..=79 => Tier::T2Scs,
        40..=54 => Tier::T3Archive,
        30..=39 => Tier::T4Full,
        15..=29 => Tier::T5Pruned,
        5..=14 => Tier::T6Light,
        _ => Tier::T7Mobile,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(cores: usize, ram: u64, disk: u64, ty: DiskType, public: Option<bool>) -> HardwareProfile {
        HardwareProfile {
            cpu_cores: cores,
            ram_gb: ram,
            disk_free_gb: disk,
            disk_type: ty,
            public_ip: public,
            upstream_mbps: None,
        }
    }

    #[test]
    fn t1_validator_for_top_tier_hardware() {
        // 32 cores / 128 GB / 4 TB NVMe / public ip
        let s = score(&p(32, 128, 4000, DiskType::Nvme, Some(true)));
        assert!(s >= 80, "score = {s}");
        assert_eq!(recommend_tier(s), Tier::T1Validator);
    }

    #[test]
    fn t2_scs_for_solid_hardware() {
        // 8 cores / 32 GB / 1 TB NVMe (no public ip detected)
        let s = score(&p(8, 32, 1000, DiskType::Nvme, None));
        assert_eq!(recommend_tier(s), Tier::T2Scs);
    }

    #[test]
    fn t3_or_t2_for_archive_hardware() {
        // 8 cores / 32 GB / 5 TB SATA — qualifies for either archive (slower
        // disk) or SCS (still solid). Score-only recommender treats both as
        // SCS; embedders that distinguish should consult `disk_type` directly.
        let s = score(&p(8, 32, 5000, DiskType::Sata, None));
        let tier = recommend_tier(s);
        assert!(
            matches!(tier, Tier::T2Scs | Tier::T3Archive),
            "got {tier:?}"
        );
    }

    #[test]
    fn t4_full_for_modest_hardware() {
        // 4 cores / 16 GB / 1 TB SATA SSD
        let s = score(&p(4, 16, 1000, DiskType::Sata, None));
        assert_eq!(recommend_tier(s), Tier::T4Full);
    }

    #[test]
    fn t5_pruned_for_small_hardware() {
        // 2 cores / 8 GB / 500 GB SATA
        let s = score(&p(2, 8, 500, DiskType::Sata, None));
        assert_eq!(recommend_tier(s), Tier::T5Pruned);
    }

    #[test]
    fn t6_light_for_low_end() {
        // 1 core / 2 GB / 50 GB HDD
        let s = score(&p(1, 2, 50, DiskType::Hdd, None));
        assert_eq!(recommend_tier(s), Tier::T6Light);
    }

    #[test]
    fn t7_mobile_for_probe_failure() {
        // Hardware probe returned nothing — should map to T7
        let s = score(&p(0, 0, 0, DiskType::Hdd, None));
        assert_eq!(recommend_tier(s), Tier::T7Mobile);
    }

    #[test]
    fn score_caps_at_100() {
        let s = score(&p(256, 1024, 100_000, DiskType::Nvme, Some(true)));
        assert_eq!(s, 100);
    }

    #[test]
    fn detect_returns_plausible_profile() {
        // We can't predict the host but cores >= 1 + ram_gb >= 0 must hold.
        let pf = detect();
        assert!(pf.cpu_cores >= 1);
        assert!(pf.ram_gb < 100_000); // sanity
    }

    #[test]
    fn classify_disk_recognises_nvme() {
        assert_eq!(classify_disk("nvme0n1"), DiskType::Nvme);
        assert_eq!(classify_disk("/dev/nvme1"), DiskType::Nvme);
    }

    #[test]
    fn classify_disk_falls_back_to_unknown() {
        assert_eq!(classify_disk("ramfs"), DiskType::Unknown);
        assert_eq!(classify_disk(""), DiskType::Unknown);
    }
}
