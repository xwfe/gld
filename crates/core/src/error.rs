use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<AppError> for gld_protocol::daemon::RpcError {
    fn from(error: AppError) -> Self {
        Self::app(error.to_string())
    }
}

impl From<gld_protocol::service::ParseServiceKindError> for AppError {
    fn from(error: gld_protocol::service::ParseServiceKindError) -> Self {
        Self::Message(error.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
