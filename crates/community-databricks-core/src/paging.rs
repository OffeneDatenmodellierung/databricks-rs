//! Pagination as a [`Stream`].
//!
//! Mirrors Go's `listing.NewIterator`: fetch a page, yield its items, and
//! let a per-endpoint step move the request on to the next page. Generated
//! clients use three strategies:
//!
//! * **token**: send back `next_page_token` until it is empty;
//! * **offset** (SCIM): start at `startIndex=1` and advance by the items
//!   seen, until a page comes back empty;
//! * **page number** (legacy SQL): start at page 1, until a page is empty.
//!
//! Pages are fetched lazily as the stream is polled.

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
/// * `step` takes the request that produced a response and the response,
///   updates the request for the next page (token, offset or page number)
///   and returns the page's items plus whether another page should be
///   fetched.
pub fn paginate<'a, Req, Resp, T, F, Fut>(
    request: Req,
    fetch: F,
    step: fn(&mut Req, Resp) -> (Vec<T>, bool),
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
            let (items, more) = step(&mut req, resp.await?);
            let next_state = more.then_some(req);
            Ok(Some((items, next_state)))
        }
    });
    pages
        .map_ok(|items| stream::iter(items.into_iter().map(Ok)))
        .try_flatten()
        .boxed()
}

/// Step function for token pagination: `token` is the response's next-page
/// token; continue while it is non-empty.
pub fn next_token(token: Option<String>, set: impl FnOnce(String)) -> bool {
    match token.filter(|t| !t.is_empty()) {
        Some(t) => {
            set(t);
            true
        }
        None => false,
    }
}

/// Run an async step on each item, in order, one at a time (for Go's
/// expanding iterators, which fetch the rest of an item as it is reached).
/// The first error ends the stream.
pub fn then_each<'a, T, U, F, Fut>(s: Paged<'a, T>, f: F) -> Paged<'a, U>
where
    T: Send + 'a,
    U: Send + 'a,
    F: FnMut(T) -> Fut + Send + 'a,
    Fut: Future<Output = Result<U>> + Send + 'a,
{
    s.and_then(f).boxed()
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
    async fn then_each_runs_in_order_and_stops_at_the_first_error() {
        let s: Paged<'_, i32> = stream::iter([Ok(1), Ok(2), Ok(3)]).boxed();
        let out: Vec<i32> = collect(then_each(s, |n| async move { Ok(n * 10) }))
            .await
            .unwrap();
        assert_eq!(out, vec![10, 20, 30]);
        let s: Paged<'_, i32> = stream::iter([Ok(1), Ok(2)]).boxed();
        let err = collect(then_each(s, |n| async move {
            if n == 2 {
                Err(crate::Error::Config("boom".into()))
            } else {
                Ok(n)
            }
        }))
        .await;
        assert!(err.is_err());
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
            |r: &mut Req, (items, token): (Vec<i32>, Option<String>)| {
                let more = next_token(token, |t| r.token = Some(t));
                (items, more)
            },
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
            |_r: &mut Req, (items, _): (Vec<i32>, Option<String>)| (items, false),
        );
        assert!(collect(s).await.is_err());
    }
}
