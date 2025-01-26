use std::sync::Arc;

use conduwuit::{implement, Result};
use futures::{FutureExt, Stream, StreamExt, TryFutureExt, TryStreamExt};
use rocksdb::Direction;
use serde::Deserialize;
use tokio::task;

use crate::{keyval, keyval::KeyVal, stream};

/// Iterate key-value entries in the map from the beginning.
///
/// - Result is deserialized
#[implement(super::Map)]
pub fn stream<'a, K, V>(
	self: &'a Arc<Self>,
) -> impl Stream<Item = Result<KeyVal<'_, K, V>>> + Send
where
	K: Deserialize<'a> + Send,
	V: Deserialize<'a> + Send,
{
	self.raw_stream().map(keyval::result_deserialize::<K, V>)
}

/// Iterate key-value entries in the map from the beginning.
///
/// - Result is raw
#[implement(super::Map)]
#[tracing::instrument(skip(self), fields(%self), level = "trace")]
pub fn raw_stream(self: &Arc<Self>) -> impl Stream<Item = Result<KeyVal<'_>>> + Send {
	use crate::pool::Seek;

	let opts = super::iter_options_default(&self.db);
	let state = stream::State::new(self, opts);
	if is_cached(self) {
		let state = state.init_fwd(None);
		return task::consume_budget()
			.map(move |()| stream::Items::<'_>::from(state))
			.into_stream()
			.flatten()
			.boxed();
	};

	let seek = Seek {
		map: self.clone(),
		dir: Direction::Forward,
		state: crate::pool::into_send_seek(state),
		key: None,
		res: None,
	};

	self.db
		.pool
		.execute_iter(seek)
		.ok_into::<stream::Items<'_>>()
		.into_stream()
		.try_flatten()
		.boxed()
}

#[tracing::instrument(
    name = "cached",
    level = "trace",
    skip_all,
    fields(%map),
)]
pub(super) fn is_cached(map: &Arc<super::Map>) -> bool {
	let opts = super::cache_iter_options_default(&map.db);
	let state = stream::State::new(map, opts).init_fwd(None);

	!state.is_incomplete()
}
