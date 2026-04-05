//! SyfrahError → HTTP JSON error response.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::error::SyfrahError;

/// Wrapper to convert SyfrahError into an axum response.
pub struct ApiError(pub SyfrahError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::Json(serde_json::json!({
            "code": self.0.code.as_str(),
            "message": self.0.message,
            "suggestion": self.0.suggestion,
        }));
        (status, body).into_response()
    }
}

impl From<SyfrahError> for ApiError {
    fn from(e: SyfrahError) -> Self {
        Self(e)
    }
}

/// Standard API success response.
#[derive(Debug, serde::Serialize)]
pub struct ApiSuccess<T: serde::Serialize> {
    pub data: T,
}

impl<T: serde::Serialize> IntoResponse for ApiSuccess<T> {
    fn into_response(self) -> Response {
        (StatusCode::OK, axum::Json(self)).into_response()
    }
}

/// API response type — either success or error.
pub type ApiResult<T> = Result<axum::Json<T>, ApiError>;

/// Standard list response with metadata.
#[derive(Debug, serde::Serialize)]
pub struct ListResponse<T: serde::Serialize> {
    pub items: Vec<T>,
    pub count: usize,
}

impl<T: serde::Serialize> ListResponse<T> {
    pub fn new(items: Vec<T>) -> Self {
        let count = items.len();
        Self { items, count }
    }
}

/// Standard create/get response.
#[derive(Debug, serde::Serialize)]
pub struct ResourceResponse<T: serde::Serialize> {
    #[serde(flatten)]
    pub resource: T,
}

/// Standard delete response.
#[derive(Debug, serde::Serialize)]
pub struct DeleteResponse {
    pub deleted: bool,
    pub message: String,
}

/// Standard action response.
#[derive(Debug, serde::Serialize)]
pub struct ActionResponse {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ActionResponse {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: None,
        }
    }

    pub fn ok_with_data(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            ok: true,
            message: message.into(),
            data: Some(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ErrorCode, SyfrahError};

    #[test]
    fn api_error_has_correct_status() {
        let err = ApiError(SyfrahError::not_found("vpc", "web"));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn api_error_conflict() {
        let err = ApiError(SyfrahError::already_exists("org", "acme"));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn api_error_validation() {
        let err = ApiError(SyfrahError::validation("bad input"));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn api_error_rate_limited() {
        let err = ApiError(SyfrahError::rate_limited());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn api_error_internal() {
        let err = ApiError(SyfrahError::internal("oops"));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn api_error_from_syfrah_error() {
        let se = SyfrahError::not_found("vm", "web-1");
        let ae: ApiError = se.into();
        assert_eq!(ae.0.code, ErrorCode::ResourceNotFound);
    }

    #[test]
    fn list_response() {
        let resp = ListResponse::new(vec!["a", "b", "c"]);
        assert_eq!(resp.count, 3);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"count\":3"));
    }

    #[test]
    fn action_response_ok() {
        let resp = ActionResponse::ok("done");
        assert!(resp.ok);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("data")); // skipped when None
    }

    #[test]
    fn action_response_with_data() {
        let resp = ActionResponse::ok_with_data("created", serde_json::json!({"id": "vpc-01"}));
        assert!(resp.data.is_some());
    }

    #[test]
    fn delete_response_serializes() {
        let resp = DeleteResponse {
            deleted: true,
            message: "vpc 'web' deleted.".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("deleted"));
    }
}
