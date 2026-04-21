use async_trait::async_trait;

use crate::MarketUpdate;

/// Data feed source: historical replay, recording replay, or live stream.
#[async_trait]
pub trait Feed: Send {
    async fn next(&mut self) -> Option<MarketUpdate>;
}

#[async_trait]
impl<T> Feed for Box<T>
where
    T: Feed + ?Sized,
{
    async fn next(&mut self) -> Option<MarketUpdate> {
        (**self).next().await
    }
}
