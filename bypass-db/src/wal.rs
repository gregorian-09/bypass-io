use std::error::Error;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// WAL magic bytes.
pub const WAL_MAGIC: [u8; 4] = *b"BYPW";

const HEADER_LEN: usize = 16;
const TRAILER_LEN: usize = 4;

/// Write-ahead-log record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalRecord {
    seq_no: u64,
    payload: Vec<u8>,
}

impl WalRecord {
    /// Create a WAL record.
    #[must_use]
    pub fn new(seq_no: u64, payload: Vec<u8>) -> Self {
        Self { seq_no, payload }
    }

    /// Record sequence number.
    #[must_use]
    pub fn seq_no(&self) -> u64 {
        self.seq_no
    }

    /// Record payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Encode into the project WAL record format.
    ///
    /// # Errors
    ///
    /// Returns [`WalError::PayloadTooLarge`] when the payload length does not
    /// fit in the 32-bit length field.
    pub fn encode(&self) -> Result<Vec<u8>, WalError> {
        let len: u32 = self
            .payload
            .len()
            .try_into()
            .map_err(|_| WalError::PayloadTooLarge {
                len: self.payload.len(),
            })?;
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len() + TRAILER_LEN);
        out.extend_from_slice(&WAL_MAGIC);
        out.extend_from_slice(&self.seq_no.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&self.payload);
        let checksum = checksum32(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        Ok(out)
    }

    fn decode_from(reader: &mut impl Read) -> Result<Option<Self>, WalError> {
        let mut header = [0u8; HEADER_LEN];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(WalError::Io(err.to_string())),
        }

        if header[0..4] != WAL_MAGIC {
            return Ok(None);
        }
        let seq_no = u64::from_le_bytes(header[4..12].try_into().expect("fixed header slice"));
        let len = u32::from_le_bytes(header[12..16].try_into().expect("fixed header slice"));
        let len = len as usize;
        let mut payload = vec![0u8; len];
        reader
            .read_exact(&mut payload)
            .map_err(|err| WalError::Io(err.to_string()))?;
        let mut trailer = [0u8; TRAILER_LEN];
        reader
            .read_exact(&mut trailer)
            .map_err(|err| WalError::Io(err.to_string()))?;
        let stored = u32::from_le_bytes(trailer);

        let mut checksum_input = Vec::with_capacity(HEADER_LEN + payload.len());
        checksum_input.extend_from_slice(&header);
        checksum_input.extend_from_slice(&payload);
        let actual = checksum32(&checksum_input);
        if stored != actual {
            return Err(WalError::ChecksumMismatch { seq_no });
        }

        Ok(Some(Self { seq_no, payload }))
    }
}

/// WAL writer.
#[derive(Debug)]
pub struct WalWriter {
    path: PathBuf,
    file: File,
    next_seq_no: u64,
}

impl WalWriter {
    /// Open a WAL writer.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the file cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)
            .map_err(|err| WalError::Io(err.to_string()))?;
        let next_seq_no = WalReader::open(&path)?.records()?.len() as u64;
        Ok(Self {
            path,
            file,
            next_seq_no,
        })
    }

    /// WAL path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a payload to the WAL.
    ///
    /// # Errors
    ///
    /// Returns a serialization or I/O error.
    pub fn append(&mut self, payload: &[u8]) -> Result<u64, WalError> {
        let seq_no = self.next_seq_no;
        self.next_seq_no = self
            .next_seq_no
            .checked_add(1)
            .ok_or(WalError::SequenceOverflow)?;
        let record = WalRecord::new(seq_no, payload.to_vec());
        let encoded = record.encode()?;
        self.file
            .write_all(&encoded)
            .map_err(|err| WalError::Io(err.to_string()))?;
        Ok(seq_no)
    }

    /// Flush the WAL to durable storage.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `sync_all` fails.
    pub fn sync(&self) -> Result<(), WalError> {
        self.file
            .sync_all()
            .map_err(|err| WalError::Io(err.to_string()))
    }
}

/// WAL reader.
#[derive(Debug)]
pub struct WalReader {
    path: PathBuf,
}

impl WalReader {
    /// Open a WAL reader.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the file cannot be opened or created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();
        OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(|err| WalError::Io(err.to_string()))?;
        Ok(Self { path })
    }

    /// Read valid records from the WAL.
    ///
    /// # Errors
    ///
    /// Returns checksum or I/O errors. Clean EOF and invalid magic stop the
    /// scan, matching crash-recovery behavior where the first invalid trailing
    /// record marks the end of the valid log.
    pub fn records(&self) -> Result<Vec<WalRecord>, WalError> {
        let mut file = File::open(&self.path).map_err(|err| WalError::Io(err.to_string()))?;
        let mut records = Vec::new();
        while let Some(record) = WalRecord::decode_from(&mut file)? {
            records.push(record);
        }
        Ok(records)
    }
}

/// WAL error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WalError {
    /// I/O error.
    Io(String),
    /// Payload length does not fit in the WAL length field.
    PayloadTooLarge {
        /// Payload length.
        len: usize,
    },
    /// Sequence number overflow.
    SequenceOverflow,
    /// Stored checksum did not match actual checksum.
    ChecksumMismatch {
        /// Record sequence number.
        seq_no: u64,
    },
}

impl fmt::Display for WalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "WAL I/O error: {err}"),
            Self::PayloadTooLarge { len } => write!(f, "WAL payload too large: {len} bytes"),
            Self::SequenceOverflow => write!(f, "WAL sequence number overflow"),
            Self::ChecksumMismatch { seq_no } => {
                write!(f, "WAL checksum mismatch at sequence {seq_no}")
            }
        }
    }
}

impl Error for WalError {}

fn checksum32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{WalReader, WalRecord, WalWriter};

    static NEXT_WAL: AtomicUsize = AtomicUsize::new(0);

    fn wal_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "bypass-db-{name}-{}-{}",
            process::id(),
            NEXT_WAL.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn record_round_trips_through_file() {
        let path = wal_path("round-trip");
        let mut writer = WalWriter::open(&path).unwrap();
        assert_eq!(writer.append(b"one").unwrap(), 0);
        assert_eq!(writer.append(b"two").unwrap(), 1);
        writer.sync().unwrap();

        let records = WalReader::open(&path).unwrap().records().unwrap();
        assert_eq!(
            records,
            vec![
                WalRecord::new(0, b"one".to_vec()),
                WalRecord::new(1, b"two".to_vec())
            ]
        );

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn checksum_detects_corruption() {
        let path = wal_path("corrupt");
        let mut writer = WalWriter::open(&path).unwrap();
        writer.append(b"payload").unwrap();
        drop(writer);

        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, bytes).unwrap();

        assert!(WalReader::open(&path).unwrap().records().is_err());
        fs::remove_file(path).unwrap();
    }
}
