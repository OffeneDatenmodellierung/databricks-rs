//! Binary request and response bodies (Go's `io.ReadCloser` fields).
//!
//! A [`Binary`] is either buffered bytes or a stream. Buffered bodies are
//! cheap to clone and are replayed on retry. A stream is consumed by the
//! first attempt, so a call with a streamed body is never retried, and a
//! streamed response is read once, by whoever holds it.

use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub use bytes::Bytes;
use bytes::BytesMut;
use futures_core::Stream;
use futures_util::{StreamExt, TryStreamExt, stream};

use crate::error::{Error, Result};

/// A stream of body chunks.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

/// A binary body: buffered bytes, or a stream that can be taken once.
#[derive(Clone)]
pub struct Binary(Inner);

#[derive(Clone)]
enum Inner {
    Bytes(Bytes),
    Stream(Arc<Mutex<Option<ByteStream>>>),
}

impl Default for Binary {
    fn default() -> Self {
        Self(Inner::Bytes(Bytes::new()))
    }
}

impl Binary {
    /// Wrap a stream of chunks, such as a file being read.
    pub fn from_stream<S, E>(s: S) -> Self
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Send + 'static,
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let s: ByteStream = s.map_err(|e| Error::Body(e.into())).boxed();
        Self(Inner::Stream(Arc::new(Mutex::new(Some(s)))))
    }

    /// The bytes, when buffered.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&Bytes> {
        match &self.0 {
            Inner::Bytes(b) => Some(b),
            Inner::Stream(_) => None,
        }
    }

    /// Whether this is a stream (which can be read only once).
    #[must_use]
    pub fn is_stream(&self) -> bool {
        matches!(self.0, Inner::Stream(_))
    }

    /// Read the whole body into memory.
    pub async fn bytes(self) -> Result<Bytes> {
        match self.0 {
            Inner::Bytes(b) => Ok(b),
            Inner::Stream(_) => {
                let mut s = self.into_stream();
                let mut buf = BytesMut::new();
                while let Some(chunk) = s.next().await {
                    buf.extend_from_slice(&chunk?);
                }
                Ok(buf.freeze())
            }
        }
    }

    /// The body as a stream of chunks. A stream can be taken once; taking
    /// it again (from a clone) yields an error.
    #[must_use]
    pub fn into_stream(self) -> ByteStream {
        match self.0 {
            Inner::Bytes(b) => stream::iter([Ok(b)]).boxed(),
            Inner::Stream(cell) => match take(&cell) {
                Some(s) => s,
                None => stream::iter([Err(consumed())]).boxed(),
            },
        }
    }

    /// The body for one request attempt: buffered bytes are cloned, a
    /// stream is taken (so it errors if an attempt already used it).
    pub(crate) fn to_request_body(&self) -> Result<reqwest::Body> {
        match &self.0 {
            Inner::Bytes(b) => Ok(reqwest::Body::from(b.clone())),
            Inner::Stream(cell) => take(cell)
                .map(reqwest::Body::wrap_stream)
                .ok_or_else(consumed),
        }
    }
}

fn take(cell: &Mutex<Option<ByteStream>>) -> Option<ByteStream> {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

fn consumed() -> Error {
    Error::Body("the body stream was already read".into())
}

impl fmt::Debug for Binary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Inner::Bytes(b) => write!(f, "Binary({} bytes)", b.len()),
            Inner::Stream(_) => f.write_str("Binary(<stream>)"),
        }
    }
}

/// Buffered bodies compare by content; streams only equal themselves.
impl PartialEq for Binary {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Inner::Bytes(a), Inner::Bytes(b)) => a == b,
            (Inner::Stream(a), Inner::Stream(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl From<Bytes> for Binary {
    fn from(b: Bytes) -> Self {
        Self(Inner::Bytes(b))
    }
}

impl From<Vec<u8>> for Binary {
    fn from(b: Vec<u8>) -> Self {
        Self(Inner::Bytes(b.into()))
    }
}

impl From<String> for Binary {
    fn from(s: String) -> Self {
        Self(Inner::Bytes(s.into()))
    }
}

impl From<&'static str> for Binary {
    fn from(s: &'static str) -> Self {
        Self(Inner::Bytes(Bytes::from_static(s.as_bytes())))
    }
}

impl From<&'static [u8]> for Binary {
    fn from(s: &'static [u8]) -> Self {
        Self(Inner::Bytes(Bytes::from_static(s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks() -> Binary {
        Binary::from_stream(stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"ab")),
            Ok(Bytes::from_static(b"cd")),
        ]))
    }

    #[tokio::test]
    async fn buffered_bodies_read_and_compare_by_content() {
        let b = Binary::from("hello");
        assert_eq!(b.as_bytes().map(Bytes::len), Some(5));
        assert!(!b.is_stream());
        assert_eq!(b, Binary::from(b"hello".to_vec()));
        assert_ne!(b, Binary::from(String::from("other")));
        assert_eq!(format!("{b:?}"), "Binary(5 bytes)");
        let out: Vec<_> = b.clone().into_stream().try_collect().await.unwrap();
        assert_eq!(out, vec![Bytes::from_static(b"hello")]);
        assert_eq!(b.bytes().await.unwrap(), "hello");
        assert_eq!(Binary::default(), Binary::from(Bytes::new()));
        assert_eq!(Binary::from(&b"x"[..]).as_bytes().unwrap(), "x");
    }

    #[tokio::test]
    async fn a_stream_is_read_once() {
        let s = chunks();
        assert!(s.is_stream());
        assert!(s.as_bytes().is_none());
        assert_eq!(format!("{s:?}"), "Binary(<stream>)");
        let again = s.clone();
        assert_eq!(s, again);
        assert_ne!(s, chunks());
        assert_ne!(s, Binary::default());
        assert_eq!(s.bytes().await.unwrap(), "abcd");
        let e = again.clone().bytes().await.unwrap_err();
        assert!(e.to_string().contains("already read"), "{e}");
        assert!(again.to_request_body().is_err());
    }

    #[tokio::test]
    async fn stream_errors_surface_as_body_errors() {
        let s = Binary::from_stream(stream::iter([Err::<Bytes, _>(std::io::Error::other(
            "disk",
        ))]));
        let e = s.bytes().await.unwrap_err();
        assert!(matches!(e, Error::Body(_)));
        assert!(e.to_string().contains("disk"));
    }

    #[test]
    fn buffered_bodies_can_be_sent_repeatedly() {
        let b = Binary::from("x");
        assert!(b.to_request_body().is_ok());
        assert!(b.to_request_body().is_ok());
        assert!(chunks().to_request_body().is_ok());
    }
}
