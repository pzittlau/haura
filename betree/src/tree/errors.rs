#![allow(missing_docs, unused_doc_comments)]
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TreeError {
    #[error("Storage operation could not be performed")]
    DmuError {
        #[from]
        source: crate::data_management::DmlError,
        // TODO: Once we migrate data_management module to thiserror we may use
        // the backtrace propagation feature
        // backtrace: Backtrace,
    },
    #[error("A key of length was given")]
    EmptyKey,
    #[error("Invalid range specification")]
    InvalidRange,
}
