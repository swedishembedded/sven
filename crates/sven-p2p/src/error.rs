use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum P2pError {
    #[error("transport error: {0}")]
    Transport(String),

    #[error("discovery error: {0}")]
    Discovery(String),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("dial error: {0}")]
    Dial(String),

    #[error("no relay addresses published yet")]
    NoRelayAddrs,

    #[error("peer not found: {0}")]
    PeerNotFound(String),

    #[error("node already shut down")]
    Shutdown,

    #[error("io error: {0}")]
    Io(String),

    #[error("keypair error: {0}")]
    Keypair(String),

    #[error("signing error: {0}")]
    Signing(String),

    #[error("signature verification failed: {0}")]
    InvalidSignature(String),
}

impl From<std::io::Error> for P2pError {
    fn from(e: std::io::Error) -> Self {
        P2pError::Io(e.to_string())
    }
}
