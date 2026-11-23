//! Where the client keeps its settings and secrets, and how sizes are written.
//!
//! Two files, both TOML, in the config directory:
//! - `config.toml`: the control plane, what to back up and when, and the storage slice.
//! - `credentials`: the recovery phrase, passphrase and dashboard token, readable only by
//!   the owner. Unattended backups need the phrase, as restic needs `--password-file`, so it
//!   is kept on this machine. Anyone who can read this file can read your backups.

use crate::Retention;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_DASHBOARD_PORT: u16 = 7410;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub control_plane: String,
    /// Loopback only, with no relays. For local test grids.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    pub backup: BackupConfig,
    pub slice: SliceConfig,
    #[serde(default)]
    pub dashboard: DashboardConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupConfig {
    pub paths: Vec<PathBuf>,
    /// Hours between scheduled backups. Zero turns scheduling off.
    #[serde(default = "default_every_hours")]
    pub every_hours: u32,
    #[serde(default = "default_keep")]
    pub keep: Retention,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SliceConfig {
    pub dir: PathBuf,
    /// Bytes offered to the pool, written like "500 GB".
    #[serde(with = "size_serde")]
    pub size: u64,
    /// Upload limit for serving other members, written like "2 MB" (per second). "0" means
    /// unlimited.
    #[serde(default, with = "size_serde")]
    pub upload_limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashboardConfig {
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        DashboardConfig { port: DEFAULT_DASHBOARD_PORT }
    }
}

fn default_every_hours() -> u32 {
    24
}
fn default_port() -> u16 {
    DEFAULT_DASHBOARD_PORT
}
pub fn default_keep() -> Retention {
    Retention { last: Some(3), daily: Some(7), weekly: Some(4), monthly: Some(6) }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    pub phrase: String,
    #[serde(default)]
    pub passphrase: String,
    pub dashboard_token: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

/// The directories everything lives in. `GLOBALOBJECT_HOME` puts them all under one
/// directory, for tests and for running several accounts on one machine.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn detect() -> Result<Paths> {
        if let Some(home) = std::env::var_os("GLOBALOBJECT_HOME") {
            let home = PathBuf::from(home);
            return Ok(Paths { config_dir: home.join("config"), data_dir: home.join("data") });
        }
        let dirs = directories::ProjectDirs::from("org", "globalobject", "globalobject").ok_or_else(|| anyhow!("can't find your home directory"))?;
        Ok(Paths { config_dir: dirs.config_dir().into(), data_dir: dirs.data_dir().into() })
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn credentials_file(&self) -> PathBuf {
        self.config_dir.join("credentials")
    }
    /// Account number, remembered snapshots and the agent's records.
    pub fn state_dir(&self) -> PathBuf {
        self.data_dir.join("state")
    }
    pub fn default_slice_dir(&self) -> PathBuf {
        self.data_dir.join("slice")
    }
    pub fn is_set_up(&self) -> bool {
        self.config_file().exists()
    }

    pub fn load(&self) -> Result<(Config, Credentials)> {
        let text = std::fs::read_to_string(self.config_file())
            .with_context(|| format!("no setup found at {}: run `globalobject setup`", self.config_file().display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("can't read {}", self.config_file().display()))?;
        Ok((cfg, self.load_credentials()?))
    }

    pub fn load_credentials(&self) -> Result<Credentials> {
        let path = self.credentials_file();
        let text = std::fs::read_to_string(&path).with_context(|| format!("can't read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("can't read {}", path.display()))
    }

    pub fn save_config(&self, cfg: &Config) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        let text = format!(
            "# GlobalObject settings. `globalobject setup` wrote this; edit freely, then run\n# `globalobject service restart`.\n\n{}",
            toml::to_string_pretty(cfg)?
        );
        write_atomic(&self.config_file(), text.as_bytes(), false)
    }

    pub fn save_credentials(&self, c: &Credentials) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        let text = format!("# Your recovery phrase. Anyone with this file can read your backups. Keep a copy\n# somewhere safe and offline: without it, nobody can recover your data.\n\n{}", toml::to_string_pretty(c)?);
        write_atomic(&self.credentials_file(), text.as_bytes(), true)
    }
}

/// Write via a temporary file and rename, so a crash never leaves half a file. Secret files
/// are created owner-only from the start, never readable by others even briefly.
fn write_atomic(path: &Path, data: &[u8], secret: bool) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).with_context(|| format!("write {}", tmp.display()))?;
    f.write_all(data)?;
    f.sync_all()?;
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Parse a size such as `500GB`, `1.5 TB`, `750 GiB` or a plain byte count. Decimal units
/// are powers of 1000, as disk makers use; `KiB`, `MiB`, `GiB` and `TiB` are powers of 1024.
pub fn parse_size(s: &str) -> Result<u64> {
    let t = s.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = (t[..split].trim(), t[split..].trim().to_ascii_lowercase());
    let n: f64 = num.parse().map_err(|_| anyhow!("'{s}' isn't a size: try something like 500GB"))?;
    let mult: f64 = match unit.as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        _ => bail!("unknown unit '{unit}' in '{s}': use KB, MB, GB or TB"),
    };
    if !n.is_finite() || n < 0.0 {
        bail!("'{s}' isn't a size");
    }
    Ok((n * mult).round() as u64)
}

/// A size for people: `512 GB`, `1.5 TB`. Decimal units, as disks are sold.
pub fn format_size(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v >= 100.0 || (v - v.round()).abs() < 0.05 {
        format!("{v:.0} {}", units[i])
    } else {
        format!("{v:.1} {}", units[i])
    }
}

mod size_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        if *v == 0 {
            s.serialize_str("0")
        } else {
            s.serialize_str(&super::format_size(*v))
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        N(u64),
        S(String),
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(n),
            Raw::S(s) => super::parse_size(&s).map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("500GB").unwrap(), 500_000_000_000);
        assert_eq!(parse_size("1.5 TB").unwrap(), 1_500_000_000_000);
        assert_eq!(parse_size("2 GiB").unwrap(), 2 << 30);
        assert_eq!(parse_size("1234").unwrap(), 1234);
        assert_eq!(parse_size(" 10 mb ").unwrap(), 10_000_000);
        assert!(parse_size("lots").is_err());
        assert!(parse_size("5 parsecs").is_err());
        assert_eq!(format_size(500_000_000_000), "500 GB");
        assert_eq!(format_size(1_500_000_000_000), "1.5 TB");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(parse_size(&format_size(34_359_738_368)).unwrap() / 1_000_000_000, 34);
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { config_dir: dir.path().join("c"), data_dir: dir.path().join("d") };
        let cfg = Config {
            control_plane: "https://cp.example.org".into(),
            local: false,
            backup: BackupConfig { paths: vec!["/home/me/Documents".into()], every_hours: 24, keep: default_keep() },
            slice: SliceConfig { dir: paths.default_slice_dir(), size: 500_000_000_000, upload_limit: 0 },
            dashboard: DashboardConfig::default(),
        };
        let creds = Credentials { phrase: "a b c".into(), passphrase: String::new(), dashboard_token: "t".into() };
        paths.save_config(&cfg).unwrap();
        paths.save_credentials(&creds).unwrap();
        let (c2, k2) = paths.load().unwrap();
        assert_eq!(c2, cfg);
        assert_eq!(k2, creds);
        let text = std::fs::read_to_string(paths.config_file()).unwrap();
        assert!(text.contains("size = \"500 GB\""), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(paths.credentials_file()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
