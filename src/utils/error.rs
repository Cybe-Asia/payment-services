//! The error of the payment service, its provider/storage clients and auth helpers.
use std::{borrow::Cow, error::Error, fmt};

type Source = Box<dyn Error + Send + Sync>;

/// `Message` text is written for the caller and may be sent as `responseError`;
/// `Failed` keeps the source for the log and never reaches a client (`into_public`).
/// `Display` keeps the former `String` wording for logs.
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

impl ServiceError {
    /// Text safe for a client: the written message, or a fixed text for a failed step.
    pub fn public_message(&self) -> &str {
        match self {
            Self::Message(message) => message,
            Self::Failed { .. } => crate::utils::response::SERVER_ERROR_MESSAGE,
        }
    }

    /// What a handler may send and choose its status from: the written message, or, once the
    /// failed step is logged, the fixed server-error text, which no handler keys a status on.
    pub fn into_public(self) -> String {
        if let Self::Failed { .. } = &self {
            tracing::error!(error = %self, "request failed");
        }
        self.public_message().to_string()
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

    #[test]
    fn only_written_text_reaches_a_client() {
        assert_eq!(
            ServiceError::from("Payment not found").into_public(),
            "Payment not found"
        );
        let failed = ServiceError::failed::<String>("payment fetch failed")("db detail".into());
        assert_eq!(failed.into_public(), "Internal Server Error");
    }
}
