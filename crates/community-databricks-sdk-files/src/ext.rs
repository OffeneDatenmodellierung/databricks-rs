//! DBFS helpers from Go's `service/files/ext_utilities.go`: a file handle
//! (`open`) that reads and writes in 1 MiB blocks, `read_file`,
//! `write_file` and `recursive_list`.

use std::collections::VecDeque;
use std::ops::BitOr;

use community_databricks_core::http::{Binary, Bytes, next_chunk};
use community_databricks_core::text::{base64_decode, base64_encode};
use community_databricks_core::{Error, Result};

use crate::{
    AddBlock, Close, Create, DbfsApi, FileInfo, GetStatusRequest, ListDbfsRequest, ReadDbfsRequest,
};

/// The largest block the DBFS API reads or writes at once.
pub const MAX_DBFS_BLOCK_SIZE: usize = 1024 * 1024;

/// How to open a DBFS file (Go: `files.FileMode`). Combine with `|`:
/// `FileMode::WRITE | FileMode::OVERWRITE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileMode(u8);

impl FileMode {
    /// Open for reading.
    pub const READ: Self = Self(1);
    /// Open for writing (creating the file).
    pub const WRITE: Self = Self(2);
    /// With `WRITE`, replace an existing file.
    pub const OVERWRITE: Self = Self(4);

    /// Whether every flag in `other` is set.
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for FileMode {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// An open DBFS file (Go: `files.Handle`), for reading or for writing.
#[derive(Debug)]
pub struct DbfsHandle {
    api: DbfsApi,
    path: String,
    reader: Option<Reader>,
    writer: Option<i64>,
}

#[derive(Debug)]
struct Reader {
    size: i64,
    offset: i64,
}

impl DbfsApi {
    /// Open `path` for reading or writing (Go: `DbfsAPI.Open`). Exactly one
    /// of [`FileMode::READ`] and [`FileMode::WRITE`] must be set.
    pub async fn open(&self, path: &str, mode: FileMode) -> Result<DbfsHandle> {
        let read = mode.contains(FileMode::READ);
        let write = mode.contains(FileMode::WRITE);
        if read == write {
            return Err(Error::OperationFailed(
                "dbfs open: must specify FileMode::READ or FileMode::WRITE".into(),
            ));
        }
        let mut handle = DbfsHandle {
            api: self.clone(),
            path: path.to_owned(),
            reader: None,
            writer: None,
        };
        let opened = if read {
            self.get_status(GetStatusRequest::new(path))
                .await
                .and_then(|info| {
                    if info.is_dir.unwrap_or(false) {
                        return Err(Error::OperationFailed(
                            "cannot open directory for reading".into(),
                        ));
                    }
                    handle.reader = Some(Reader {
                        size: info.file_size.unwrap_or(0),
                        offset: 0,
                    });
                    Ok(())
                })
        } else {
            self.create(Create::new(path).with_overwrite(mode.contains(FileMode::OVERWRITE)))
                .await
                .map(|res| handle.writer = Some(res.handle.unwrap_or_default()))
        };
        opened.map_err(|e| Error::OperationFailed(format!("dbfs open: {e}")))?;
        Ok(handle)
    }

    /// The whole contents of `name` (Go: `DbfsAPI.ReadFile`).
    pub async fn read_file(&self, name: &str) -> Result<Vec<u8>> {
        self.open(name, FileMode::READ).await?.read_all().await
    }

    /// Write `data` to `name`, replacing it (Go: `DbfsAPI.WriteFile`). The
    /// handle is closed even if a write fails.
    pub async fn write_file(&self, name: &str, data: &[u8]) -> Result<()> {
        let mut h = self
            .open(name, FileMode::WRITE | FileMode::OVERWRITE)
            .await?;
        let written = h.write(data).await;
        let closed = h.close().await;
        written.and(closed)
    }

    /// Every file under `path`, breadth first; directories that vanish
    /// while listing are skipped (Go: `DbfsAPI.RecursiveList`).
    pub async fn recursive_list(&self, path: &str) -> Result<Vec<FileInfo>> {
        let mut results = Vec::new();
        let mut queue = VecDeque::from([path.to_owned()]);
        while let Some(dir) = queue.pop_front() {
            let batch = match self.list_all(ListDbfsRequest::new(dir.clone())).await {
                Ok(b) => b,
                Err(e) if e.is_missing() => continue,
                Err(e) => return Err(Error::OperationFailed(format!("list {dir}: {e}"))),
            };
            for v in batch {
                if v.is_dir.unwrap_or(false) {
                    queue.push_back(v.path.clone().unwrap_or_default());
                } else {
                    results.push(v);
                }
            }
        }
        Ok(results)
    }
}

impl DbfsHandle {
    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The file size, for a handle open for reading.
    #[must_use]
    pub fn size(&self) -> Option<i64> {
        self.reader.as_ref().map(|r| r.size)
    }

    /// Read into `buf` from the current offset, in blocks of at most
    /// 1 MiB. Returns the bytes read; 0 at end of file.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let Some(r) = self.reader.as_mut() else {
            return Err(Error::OperationFailed(
                "dbfs: file not open for reading".into(),
            ));
        };
        let mut total = 0;
        while total < buf.len() && r.offset < r.size {
            let want = (buf.len() - total).min(MAX_DBFS_BLOCK_SIZE);
            let res = self
                .api
                .read(
                    ReadDbfsRequest::new(self.path.clone())
                        .with_length(i64::try_from(want).unwrap_or(i64::MAX))
                        .with_offset(r.offset),
                )
                .await
                .map_err(|e| Error::OperationFailed(format!("dbfs read: {e}")))?;
            if res.bytes_read.unwrap_or(0) == 0 {
                return Err(Error::OperationFailed(format!(
                    "dbfs read: unexpected EOF at offset {} (size {})",
                    r.offset, r.size
                )));
            }
            let data = base64_decode(res.data.as_deref().unwrap_or_default())
                .map_err(|e| Error::OperationFailed(format!("dbfs read: {e}")))?;
            let n = data.len().min(buf.len() - total);
            buf[total..total + n].copy_from_slice(&data[..n]);
            total += n;
            r.offset += i64::try_from(n).unwrap_or(i64::MAX);
        }
        Ok(total)
    }

    /// Read everything from the current offset to the end (Go:
    /// `Handle.WriteTo`).
    pub async fn read_all(&mut self) -> Result<Vec<u8>> {
        let remaining = self
            .reader
            .as_ref()
            .map_or(0, |r| usize::try_from(r.size - r.offset).unwrap_or(0));
        let mut buf = vec![0; remaining];
        let n = self.read(&mut buf).await?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Append `data`, in blocks of at most 1 MiB. Returns the bytes
    /// written.
    pub async fn write(&mut self, data: &[u8]) -> Result<usize> {
        let Some(handle) = self.writer else {
            return Err(Error::OperationFailed(
                "dbfs: file not open for writing".into(),
            ));
        };
        for chunk in data.chunks(MAX_DBFS_BLOCK_SIZE) {
            self.api
                .add_block(AddBlock::new(base64_encode(chunk), handle))
                .await
                .map_err(|e| Error::OperationFailed(format!("dbfs write: {e}")))?;
        }
        Ok(data.len())
    }

    /// Append everything from `body`, a stream or bytes, in 1 MiB blocks
    /// (Go: `Handle.ReadFrom`). Returns the bytes written.
    pub async fn write_from(&mut self, body: impl Into<Binary>) -> Result<u64> {
        let mut stream = body.into().into_stream();
        let mut pending: Vec<u8> = Vec::with_capacity(MAX_DBFS_BLOCK_SIZE);
        let mut total = 0u64;
        while let Some(chunk) = next_chunk(&mut stream).await {
            let chunk: Bytes = chunk?;
            pending.extend_from_slice(&chunk);
            while pending.len() >= MAX_DBFS_BLOCK_SIZE {
                let rest = pending.split_off(MAX_DBFS_BLOCK_SIZE);
                total += self.write(&pending).await? as u64;
                pending = rest;
            }
        }
        if !pending.is_empty() {
            total += self.write(&pending).await? as u64;
        }
        Ok(total)
    }

    /// Close a handle open for writing (Go: `Handle.Close`).
    pub async fn close(self) -> Result<()> {
        let Some(handle) = self.writer else {
            return Err(Error::OperationFailed(
                "dbfs: file not open for writing".into(),
            ));
        };
        self.api
            .close(Close::new(handle))
            .await
            .map_err(|e| Error::OperationFailed(format!("dbfs write: {e}")))
    }
}
