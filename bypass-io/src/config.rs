//! Runtime configuration for `bypass-io`.
//!
//! The project specification describes a TOML configuration file named
//! `bypass-io.toml`. This module provides the typed model, serde-backed TOML
//! parsing, deterministic TOML generation, and validation for invariants that
//! can be checked before hardware-specific runtime setup begins.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

/// Complete runtime configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BypassConfig {
    /// Poll-reactor CPU configuration.
    pub reactor: ReactorConfig,
    /// Huge-page buffer-pool configuration.
    pub bufpool: BufPoolConfig,
    /// `io_uring` backend configuration.
    pub uring: UringConfig,
    /// SPDK backend configuration.
    pub spdk: SpdkRuntimeConfig,
    /// DPDK backend configuration.
    pub dpdk: DpdkRuntimeConfig,
    /// Embedded database configuration.
    pub db: DbConfig,
}

impl BypassConfig {
    /// Load configuration from a TOML file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, parsed, or validated.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
        Self::from_toml_str(&text)
    }

    /// Parse a `bypass-io.toml` string.
    ///
    /// # Errors
    ///
    /// Returns an error when TOML deserialization fails or validation rejects
    /// the resulting values.
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let config: Self =
            toml::from_str(input).map_err(|source| ConfigError::Deserialize(source.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Serialize the configuration to deterministic spec-shaped TOML.
    #[must_use]
    pub fn to_toml_string(&self) -> String {
        let mut out = String::new();
        out.push_str("[reactor]\n");
        out.push_str(&format!(
            "poll_cores = [{}]\n\n",
            join_numbers(&self.reactor.poll_cores)
        ));
        out.push_str("[bufpool]\n");
        out.push_str(&format!("count = {}\n", self.bufpool.count));
        out.push_str(&format!("buf_size_mib = {}\n\n", self.bufpool.buf_size_mib));
        out.push_str("[uring]\n");
        out.push_str(&format!("sq_depth = {}\n", self.uring.sq_depth));
        out.push_str(&format!("cq_depth = {}\n", self.uring.cq_depth));
        out.push_str(&format!(
            "sqpoll_idle_ms = {}\n\n",
            self.uring.sqpoll_idle_ms
        ));
        out.push_str("[spdk]\n");
        out.push_str(&format!(
            "devices = [{}]\n",
            join_strings(&self.spdk.devices)
        ));
        out.push_str(&format!("queue_depth = {}\n", self.spdk.queue_depth));
        out.push_str(&format!("io_size_mib = {}\n\n", self.spdk.io_size_mib));
        out.push_str("[dpdk]\n");
        out.push_str(&format!("pci_addr = {:?}\n", self.dpdk.pci_addr));
        out.push_str(&format!("rx_queues = {}\n", self.dpdk.rx_queues));
        out.push_str(&format!("tx_queues = {}\n", self.dpdk.tx_queues));
        out.push_str(&format!("rss_key = {:?}\n\n", self.dpdk.rss_key));
        out.push_str("[db]\n");
        out.push_str(&format!("path = {:?}\n", self.db.path));
        out.push_str(&format!("wal_size_mib = {}\n", self.db.wal_size_mib));
        out.push_str(&format!("segment_rows = {}\n", self.db.segment_rows));
        out.push_str(&format!(
            "compaction_threads = {}\n\n",
            self.db.compaction_threads
        ));
        out.push_str("[db.schema]\n");
        out.push_str(&format!("name = {:?}\n", self.db.schema.name));
        for column in &self.db.schema.columns {
            out.push_str("\n[[db.schema.columns]]\n");
            out.push_str(&format!("name = {:?}\n", column.name));
            out.push_str(&format!("dtype = {}\n", column.dtype.to_toml_value()));
        }
        out
    }

    /// Validate cross-field invariants.
    ///
    /// # Errors
    ///
    /// Returns a validation error for zero capacities, missing names, duplicate
    /// columns, or invalid schema shape.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.reactor.poll_cores.is_empty() {
            return validation("reactor.poll_cores must not be empty");
        }
        if self.bufpool.count == 0 {
            return validation("bufpool.count must be greater than zero");
        }
        if self.bufpool.buf_size_mib == 0 {
            return validation("bufpool.buf_size_mib must be greater than zero");
        }
        if self.uring.sq_depth == 0 || self.uring.cq_depth == 0 {
            return validation("uring queue depths must be greater than zero");
        }
        if self.spdk.queue_depth == 0 {
            return validation("spdk.queue_depth must be greater than zero");
        }
        if self.spdk.io_size_mib == 0 {
            return validation("spdk.io_size_mib must be greater than zero");
        }
        if self.dpdk.pci_addr.is_empty() {
            return validation("dpdk.pci_addr must not be empty");
        }
        if self.dpdk.rx_queues == 0 || self.dpdk.tx_queues == 0 {
            return validation("dpdk queue counts must be greater than zero");
        }
        if self.db.path.is_empty() {
            return validation("db.path must not be empty");
        }
        if self.db.wal_size_mib == 0 {
            return validation("db.wal_size_mib must be greater than zero");
        }
        if self.db.segment_rows == 0 {
            return validation("db.segment_rows must be greater than zero");
        }
        if self.db.compaction_threads == 0 {
            return validation("db.compaction_threads must be greater than zero");
        }
        self.db.schema.validate()
    }
}

/// Poll-reactor CPU configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReactorConfig {
    /// CPU ids dedicated to polling.
    pub poll_cores: Vec<u32>,
}

impl Default for ReactorConfig {
    fn default() -> Self {
        Self {
            poll_cores: vec![0, 1],
        }
    }
}

/// Huge-page buffer-pool configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BufPoolConfig {
    /// Number of buffers to pre-allocate.
    pub count: usize,
    /// Size of each buffer in MiB.
    pub buf_size_mib: usize,
}

impl Default for BufPoolConfig {
    fn default() -> Self {
        Self {
            count: 512,
            buf_size_mib: 2,
        }
    }
}

/// `io_uring` runtime configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UringConfig {
    /// Submission queue depth.
    pub sq_depth: u32,
    /// Completion queue depth.
    pub cq_depth: u32,
    /// SQPOLL idle timeout in milliseconds.
    pub sqpoll_idle_ms: u32,
}

impl Default for UringConfig {
    fn default() -> Self {
        Self {
            sq_depth: 4096,
            cq_depth: 8192,
            sqpoll_idle_ms: 2,
        }
    }
}

/// SPDK runtime configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpdkRuntimeConfig {
    /// PCI BDFs for NVMe devices.
    pub devices: Vec<String>,
    /// Per-namespace queue depth.
    pub queue_depth: u32,
    /// Maximum single I/O size in MiB.
    pub io_size_mib: usize,
}

impl Default for SpdkRuntimeConfig {
    fn default() -> Self {
        Self {
            devices: vec!["0000:01:00.0".to_string()],
            queue_depth: 256,
            io_size_mib: 128,
        }
    }
}

/// DPDK runtime configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DpdkRuntimeConfig {
    /// PCI BDF for the NIC.
    pub pci_addr: String,
    /// RX queue count.
    pub rx_queues: u16,
    /// TX queue count.
    pub tx_queues: u16,
    /// RSS hash key encoded as text.
    pub rss_key: String,
}

impl Default for DpdkRuntimeConfig {
    fn default() -> Self {
        Self {
            pci_addr: "0000:02:00.0".to_string(),
            rx_queues: 1,
            tx_queues: 1,
            rss_key: "6d5a56da".to_string(),
        }
    }
}

/// Embedded database configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DbConfig {
    /// Table root path.
    pub path: String,
    /// WAL rotation target in MiB.
    pub wal_size_mib: usize,
    /// Row count at which active segments should be sealed.
    pub segment_rows: usize,
    /// Number of compaction worker threads.
    pub compaction_threads: usize,
    /// Table schema.
    pub schema: DbSchemaConfig,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            path: "/data/bypass-db".to_string(),
            wal_size_mib: 256,
            segment_rows: 1_000_000,
            compaction_threads: 1,
            schema: DbSchemaConfig::default(),
        }
    }
}

/// Database schema configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DbSchemaConfig {
    /// Table name.
    pub name: String,
    /// Ordered columns.
    pub columns: Vec<DbColumnConfig>,
}

impl DbSchemaConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.name.is_empty() {
            return validation("db.schema.name must not be empty");
        }
        if self.columns.is_empty() {
            return validation("db.schema.columns must not be empty");
        }

        let mut timestamp_count = 0usize;
        for (idx, column) in self.columns.iter().enumerate() {
            if column.name.is_empty() {
                return validation("db.schema.columns.name must not be empty");
            }
            if self
                .columns
                .iter()
                .take(idx)
                .any(|seen| seen.name == column.name)
            {
                return validation("db.schema column names must be unique");
            }
            if column.dtype == DbColumnDType::Timestamp {
                timestamp_count += 1;
            }
            if let DbColumnDType::FixedStr(width) = column.dtype {
                if width == 0 {
                    return validation("db.schema FixedStr width must be greater than zero");
                }
            }
        }
        if timestamp_count != 1 {
            return validation("db.schema must contain exactly one Timestamp column");
        }
        Ok(())
    }
}

impl Default for DbSchemaConfig {
    fn default() -> Self {
        Self {
            name: "trades".to_string(),
            columns: vec![
                DbColumnConfig {
                    name: "timestamp".to_string(),
                    dtype: DbColumnDType::Timestamp,
                },
                DbColumnConfig {
                    name: "symbol".to_string(),
                    dtype: DbColumnDType::FixedStr(8),
                },
                DbColumnConfig {
                    name: "price".to_string(),
                    dtype: DbColumnDType::F64,
                },
                DbColumnConfig {
                    name: "volume".to_string(),
                    dtype: DbColumnDType::F64,
                },
            ],
        }
    }
}

/// Database column configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DbColumnConfig {
    /// Column name.
    pub name: String,
    /// Column type.
    pub dtype: DbColumnDType,
}

/// Database column type used by [`DbColumnConfig`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DbColumnDType {
    /// 64-bit floating-point value.
    F64,
    /// 64-bit signed integer.
    I64,
    /// Nanosecond timestamp stored as `i64`.
    Timestamp,
    /// Fixed-width byte string.
    FixedStr(usize),
}

impl Serialize for DbColumnDType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::F64 => serializer.serialize_str("F64"),
            Self::I64 => serializer.serialize_str("I64"),
            Self::Timestamp => serializer.serialize_str("Timestamp"),
            Self::FixedStr(width) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("FixedStr", width)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for DbColumnDType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Name(String),
            FixedStr {
                #[serde(rename = "FixedStr")]
                fixed_str: usize,
            },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Name(name) if name == "F64" => Ok(Self::F64),
            Repr::Name(name) if name == "I64" => Ok(Self::I64),
            Repr::Name(name) if name == "Timestamp" => Ok(Self::Timestamp),
            Repr::Name(name) => Err(de::Error::unknown_variant(
                &name,
                &["F64", "I64", "Timestamp", "FixedStr"],
            )),
            Repr::FixedStr { fixed_str } => Ok(Self::FixedStr(fixed_str)),
        }
    }
}

impl DbColumnDType {
    fn to_toml_value(&self) -> String {
        match self {
            Self::F64 => format!("{:?}", "F64"),
            Self::I64 => format!("{:?}", "I64"),
            Self::Timestamp => format!("{:?}", "Timestamp"),
            Self::FixedStr(width) => format!("{{ FixedStr = {width} }}"),
        }
    }
}

/// Configuration error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// File I/O failed.
    Io {
        /// File path.
        path: PathBuf,
        /// Source error message.
        message: String,
    },
    /// TOML deserialization failed.
    Deserialize(String),
    /// TOML serialization failed.
    Serialize(String),
    /// Validation failed.
    Validation(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
            Self::Deserialize(message) => write!(f, "config TOML parse error: {message}"),
            Self::Serialize(message) => write!(f, "config TOML serialization error: {message}"),
            Self::Validation(message) => write!(f, "config validation error: {message}"),
        }
    }
}

impl Error for ConfigError {}

fn validation<T>(message: &str) -> Result<T, ConfigError> {
    Err(ConfigError::Validation(message.to_string()))
}

fn join_numbers(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_strings(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::{BypassConfig, ConfigError, DbColumnDType};

    #[test]
    fn default_config_round_trips_through_toml() {
        let config = BypassConfig::default();
        let parsed = BypassConfig::from_toml_str(&config.to_toml_string()).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn parser_reads_spec_shaped_config() {
        let config = BypassConfig::from_toml_str(
            r#"
[reactor]
poll_cores = [0, 1]

[bufpool]
count = 512
buf_size_mib = 2

[uring]
sq_depth = 4096
cq_depth = 8192
sqpoll_idle_ms = 2

[spdk]
devices = ["0000:01:00.0"]
queue_depth = 256
io_size_mib = 128

[dpdk]
pci_addr = "0000:02:00.0"
rx_queues = 1
tx_queues = 1
rss_key = "6d5a56da"

[db]
path = "/data/bypass-db"
wal_size_mib = 256
segment_rows = 1000000
compaction_threads = 1

[db.schema]
name = "trades"

[[db.schema.columns]]
name = "timestamp"
dtype = "Timestamp"

[[db.schema.columns]]
name = "symbol"
dtype = { FixedStr = 8 }

[[db.schema.columns]]
name = "price"
dtype = "F64"
"#,
        )
        .unwrap();

        assert_eq!(config.reactor.poll_cores, vec![0, 1]);
        assert_eq!(
            config.db.schema.columns[1].dtype,
            DbColumnDType::FixedStr(8)
        );
    }

    #[test]
    fn validation_rejects_missing_timestamp() {
        let mut config = BypassConfig::default();
        config
            .db
            .schema
            .columns
            .retain(|column| column.name != "timestamp");
        assert_eq!(
            config.validate(),
            Err(ConfigError::Validation(
                "db.schema must contain exactly one Timestamp column".to_string()
            ))
        );
    }
}
