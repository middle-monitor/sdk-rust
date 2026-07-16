use std::fmt;

#[derive(Debug)]
pub enum Error {
    NotInitialized,
    TracerUnavailable,
    Init(String),
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotInitialized => write!(f, "client not initialized"),
            Error::TracerUnavailable => write!(f, "tracer not available"),
            Error::Init(msg) => write!(f, "initialization failed: {}", msg),
            Error::Config(msg) => write!(f, "{}", msg),
        }
    }
}


impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_not_initialized() {
        assert_eq!(format!("{}", Error::NotInitialized), "client not initialized");
    }

    #[test]
    fn display_tracer_unavailable() {
        assert_eq!(format!("{}", Error::TracerUnavailable), "tracer not available");
    }

    #[test]
    fn display_init_error() {
        let e = Error::Init("bad config".to_string());
        assert_eq!(format!("{}", e), "initialization failed: bad config");
    }

    #[test]
    fn debug_format() {
        let _ = format!("{:?}", Error::NotInitialized);
        let _ = format!("{:?}", Error::TracerUnavailable);
        let _ = format!("{:?}", Error::Init("x".to_string()));
    }

    #[test]
    fn is_std_error() {
        let e: Box<dyn std::error::Error> = Box::new(Error::NotInitialized);
        assert!(!e.to_string().is_empty());
    }
}
