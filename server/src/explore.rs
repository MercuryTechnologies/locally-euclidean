//! `/explore/:bucketName/:filename` endpoint, for viewing files in the
//! browser.
//!
//! FIXME(jadel): does this want to be "just serve the files as http with no
//! UI" or does this want to be "with UI"?
use axum::{extract, routing::get};

use crate::AppState;

pub fn make_router() -> axum::Router<AppState> {
    axum::Router::new().route("/{bucket}/{*filename}", get(get_explore))
}

async fn get_explore(
    _state: extract::State<AppState>,
    extract::Path((_bucket, _filename)): extract::Path<(String, String)>,
) {
    unimplemented!("todo!")
}
