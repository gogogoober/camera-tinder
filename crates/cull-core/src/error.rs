use crate::model::PhotoId;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("unknown source: {0}")]
    UnknownSource(String),
    #[error("photo not found: {0}")]
    PhotoNotFound(PhotoId),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
