//! The error of the payment service, its provider/storage clients and auth helpers.
use std::{borrow::Cow, error::Error, fmt};

type Source = Box<dyn Error + Send + Sync>;

/// `Display` is exactly the text these paths returned as `String` before: handlers
/// still choose the HTTP status from it and send it as `responseError`.
#[derive(Debug)]
pub enum ServiceError {
    /// A refusal or failure described only by its message.
    Message(Cow<'static, str>),
    /// A storage, provider or encoding step failed; `context` names the step.
    Failed {
        context: &'static str,
        source: Source,
    },
}

impl ServiceError {
    /// `map_err` adapter that keeps the source: `.map_err(ServiceError::failed("payment fetch failed"))`.
    pub fn failed<E: Into<Source>>(context: &'static str) -> impl FnOnce(E) -> Self {
        move |source| Self::Failed {
            context,
            source: source.into(),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => f.write_str(message),
            Self::Failed { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl Error for ServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Message(_) => None,
            Self::Failed { source, .. } => Some(source.as_ref()),
        }
    }
}

impl From<&'static str> for ServiceError {
    fn from(message: &'static str) -> Self {
        Self::Message(Cow::Borrowed(message))
    }
}

impl From<String> for ServiceError {
    fn from(message: String) -> Self {
        Self::Message(Cow::Owned(message))
    }
}

#[cfg(test)]
mod tests {
    use super::ServiceError;

    #[test]
    fn display_keeps_the_former_string_text() {
        assert_eq!(
            ServiceError::from("Payment not found").to_string(),
            "Payment not found"
        );
        let failed = ServiceError::failed::<String>("payment fetch failed")("boom".into());
        assert_eq!(failed.to_string(), "payment fetch failed: boom");
        assert!(std::error::Error::source(&failed).is_some());
    }
}
