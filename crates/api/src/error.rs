//! Error responses + a JSON extractor that produces free-sleep's exact error
//! bodies:
//!
//! - malformed JSON            → `400 {"error":{"message":"Invalid JSON"}}`
//! - schema/validation failure → `400 {"error":"Invalid request data","details":[...]}`
//! - unknown `/api/*`          → `404 {"error":{"message":"Not Found"}}`

use axum::extract::FromRequest;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde_json::json;

/// `400 {"error":{"message":"Invalid JSON"}}`
pub fn invalid_json() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": { "message": "Invalid JSON" } })),
    )
        .into_response()
}

/// `400 {"error":"Invalid request data","details":[...]}`
pub fn invalid_request_data(details: Vec<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": "Invalid request data", "details": details })),
    )
        .into_response()
}

/// `404 {"error":{"message":"Not Found"}}`
pub fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": { "message": "Not Found" } })),
    )
        .into_response()
}

/// JSON extractor with free-sleep-compatible error mapping. Syntax errors map to
/// "Invalid JSON"; anything that parses as JSON but not as `T` maps to
/// "Invalid request data".
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        // First parse as an untyped Value: this distinguishes JSON syntax /
        // content-type errors (Invalid JSON) from schema mismatches.
        let Json(value) = Json::<serde_json::Value>::from_request(req, state)
            .await
            .map_err(|_| invalid_json())?;

        match serde_json::from_value::<T>(value) {
            Ok(v) => Ok(ApiJson(v)),
            Err(e) => Err(invalid_request_data(vec![e.to_string()])),
        }
    }
}
