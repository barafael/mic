//! The HTTP side of the contract `moq-relay` speaks to an auth server: `POST /` with the
//! session's [`Request`], answered with a [`Grant`] (200), nothing for an `end` (204), or a
//! refusal (403) with its reason in the body, which only the relay's log reads.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use moq_auth::Request;

use crate::policy::Admission;

pub fn router(admission: Arc<Admission>) -> Router {
    Router::new().route("/", post(handle)).with_state(admission)
}

/// Serves on `listener` until the task is dropped.
pub async fn serve(
    listener: tokio::net::TcpListener,
    admission: Arc<Admission>,
) -> std::io::Result<()> {
    axum::serve(listener, router(admission)).await
}

async fn handle(State(admission): State<Arc<Admission>>, Json(request): Json<Request>) -> Response {
    match admission.answer(&request) {
        Ok(Some(grant)) => Json(grant).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(refusal) => {
            tracing::debug!(id = %request.id, path = %request.path, %refusal, "refused");
            (StatusCode::FORBIDDEN, refusal.to_string()).into_response()
        }
    }
}
