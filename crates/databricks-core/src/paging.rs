//! Token-based pagination as a [`Stream`].
//!
//! Mirrors Go's `listing.NewIterator`: fetch a page, yield its items, and
//! request the next page while the response carries a non-empty
//! `next_page_token`. Pages are fetched lazily as the stream is polled.

use std::future::Future;
use std::pin::Pin;

use futures_core::Stream;
use futures_util::{StreamExt, TryStreamExt, stream};

use crate::error::Result;

/// A boxed stream of items from a paginated list endpoint.
pub type Paged<'a, T> = Pin<Box<dyn Stream<Item = Result<T>> + Send + 'a>>;

/// Build a paginated stream.
///
/// * `fetch` loads one page for a request.
/// * `split` turns a response into its items and the next page token
///   (`None` or empty ends the stream).
/// * `advance` writes the token into the request for the next page.
pub fn paginate<'a, Req, Resp, T, F, Fut>(
    request: Req,
    fetch: F,
    split: fn(Resp) -> (Vec<T>, Option<String>),
    advance: fn(&mut Req, String),
) -> Paged<'a, T>
where
    Req: Send + 'a,
    Resp: Send + 'a,
    T: Send + 'a,
    F: Fn(&Req) -> Fut + Send + Sync + 'a,
    Fut: Future<Output = Result<Resp>> + Send + 'a,
{
    let pages = stream::try_unfold(Some(request), move |state| {
        let fut = state.map(|req| {
            let resp = fetch(&req);
            (req, resp)
        });
        async move {
            let Some((mut req, resp)) = fut else {
                return Ok::<_, crate::Error>(None);
            };
            let (items, next) = split(resp.await?);
            let next_state = match next.filter(|t| !t.is_empty()) {
                Some(token) => {
                    advance(&mut req, token);
                    Some(req)
                }
                None => None,
            };
            Ok(Some((items, next_state)))
        }
    });
    pages
        .map_ok(|items| stream::iter(items.into_iter().map(Ok)))
        .try_flatten()
        .boxed()
}

/// Drain a paginated stream into a `Vec` (Go's `ListAll`).
pub async fn collect<T>(s: Paged<'_, T>) -> Result<Vec<T>> {
    s.try_collect().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Default)]
    struct Req {
        token: Option<String>,
    }

    #[tokio::test]
    async fn walks_pages_lazily_and_stops_on_empty_token() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let s = paginate(
            Req::default(),
            move |r: &Req| {
                c.fetch_add(1, Ordering::SeqCst);
                let page = r.token.clone();
                async move {
                    Ok::<_, crate::Error>(match page.as_deref() {
                        None => (vec![1, 2], Some("p2".to_owned())),
                        Some("p2") => (vec![], Some("p3".to_owned())),
                        Some("p3") => (vec![3], Some(String::new())),
                        _ => unreachable!(),
                    })
                }
            },
            |r| r,
            |r, t| r.token = Some(t),
        );
        let mut s = s;
        assert_eq!(s.next().await.unwrap().unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let rest: Vec<i32> = s.try_collect().await.unwrap();
        assert_eq!(rest, vec![2, 3]);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn errors_end_the_stream() {
        let s = paginate(
            Req::default(),
            |_r: &Req| async {
                Err::<(Vec<i32>, Option<String>), _>(crate::Error::OperationFailed("x".into()))
            },
            |r| r,
            |r, t| r.token = Some(t),
        );
        assert!(collect(s).await.is_err());
    }
}
