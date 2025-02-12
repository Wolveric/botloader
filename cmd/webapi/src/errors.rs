use axum::{
    body::{self, BoxBody},
    http::{header, Response, StatusCode},
    response::IntoResponse,
};
use serde_json::json;
use validation::ValidationError;

#[derive(Debug, thiserror::Error)]
pub enum ApiErrorResponse {
    #[error("csrf token expired")]
    BadCsrfToken,

    #[error("Session expired")]
    SessionExpired,

    #[error("validation failed")]
    ValidationFailed(Vec<ValidationError>),

    #[error("internal server error")]
    InternalError,

    #[error("no active guild")]
    NoActiveGuild,

    #[error("not guild admin")]
    NotGuildAdmin,

    #[error("Plugin does not exist")]
    PluginNotFound,

    #[error("you do not have access to this plugin")]
    NoAccessToPlugin,

    #[error("you have created too many plugins")]
    UserPluginLimitReached,
}

impl ApiErrorResponse {
    pub fn public_desc(&self) -> (StatusCode, u32, String) {
        match &self {
            Self::SessionExpired => (StatusCode::BAD_REQUEST, 1, self.to_string()),
            Self::BadCsrfToken => (StatusCode::BAD_REQUEST, 2, self.to_string()),
            Self::InternalError => (StatusCode::INTERNAL_SERVER_ERROR, 3, self.to_string()),
            Self::ValidationFailed(verr) => (
                StatusCode::BAD_REQUEST,
                4,
                serde_json::to_string(verr).unwrap_or_default(),
            ),
            Self::NoActiveGuild => (StatusCode::BAD_REQUEST, 5, self.to_string()),
            Self::NotGuildAdmin => (StatusCode::FORBIDDEN, 6, self.to_string()),
            Self::NoAccessToPlugin => (StatusCode::FORBIDDEN, 7, self.to_string()),
            Self::UserPluginLimitReached => (StatusCode::BAD_REQUEST, 8, self.to_string()),
            Self::PluginNotFound => (StatusCode::BAD_REQUEST, 9, self.to_string()),
        }
    }
}

impl IntoResponse for ApiErrorResponse {
    fn into_response(self) -> Response<BoxBody> {
        let (resp_code, err_code, msg) = self.public_desc();

        let body = json!({
            "code": err_code,
            "description": msg,
        })
        .to_string();

        Response::builder()
            .status(resp_code)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body::boxed(body::Full::from(body)))
            .unwrap()
    }
}
