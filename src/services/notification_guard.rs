//! Activation guard shared by background invoice, receipt and auto-invoice workers.
//! Enabling the workers must not backfill records queued while they were disabled,
//! and the first acceptance run must reach only explicitly approved recipients.
//! Records outside the guard stay queued and untouched for an explicit later decision.
use chrono::{DateTime, Utc};

pub const ACTIVATED_AT_ENV: &str = "NOTIFICATION_ACTIVATED_AT";
pub const RECIPIENTS_ENV: &str = "NOTIFICATION_RECIPIENT_ALLOWLIST";

#[derive(Clone, Debug, PartialEq)]
pub struct ActivationGuard {
    /// RFC3339 instant; only records created/paid/accepted at or after it are eligible.
    pub activated_at: String,
    /// `None` means every recipient (explicit `*`); otherwise lower-cased exact addresses.
    pub recipients: Option<Vec<String>>,
}

impl ActivationGuard {
    pub fn from_env() -> Result<Self, &'static str> {
        Self::parse(
            std::env::var(ACTIVATED_AT_ENV).ok().as_deref(),
            std::env::var(RECIPIENTS_ENV).ok().as_deref(),
        )
    }

    /// Fails closed: both values are required and must be well formed.
    pub fn parse(activated_at: Option<&str>, recipients: Option<&str>) -> Result<Self, &'static str> {
        let activated_at = activated_at
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or("NOTIFICATION_ACTIVATED_AT is required")?;
        let activated_at = DateTime::parse_from_rfc3339(activated_at)
            .map_err(|_| "NOTIFICATION_ACTIVATED_AT must be RFC3339")?
            .with_timezone(&Utc)
            .to_rfc3339();
        let recipients = recipients
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or("NOTIFICATION_RECIPIENT_ALLOWLIST is required")?;
        let recipients = if recipients == "*" {
            None
        } else {
            let list: Vec<String> = recipients
                .split(',')
                .map(|v| v.trim().to_lowercase())
                .filter(|v| !v.is_empty())
                .collect();
            if list.is_empty() || list.iter().any(|v| v == "*" || !v.contains('@')) {
                return Err("NOTIFICATION_RECIPIENT_ALLOWLIST must be * or email addresses");
            }
            Some(list)
        };
        Ok(Self {
            activated_at,
            recipients,
        })
    }

    pub fn all_recipients(&self) -> bool {
        self.recipients.is_none()
    }

    pub fn recipient_list(&self) -> Vec<String> {
        self.recipients.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_fails_closed_and_normalizes() {
        assert!(ActivationGuard::parse(None, Some("*")).is_err());
        assert!(ActivationGuard::parse(Some("2026-10-05"), Some("*")).is_err());
        assert!(ActivationGuard::parse(Some("2026-10-05T10:00:00+07:00"), None).is_err());
        assert!(ActivationGuard::parse(Some("2026-10-05T10:00:00+07:00"), Some(" ")).is_err());
        assert!(ActivationGuard::parse(Some("2026-10-05T10:00:00+07:00"), Some("a@x.test,*")).is_err());
        assert!(ActivationGuard::parse(Some("2026-10-05T10:00:00+07:00"), Some("not-an-email")).is_err());
        let all = ActivationGuard::parse(Some("2026-10-05T10:00:00+07:00"), Some("*")).unwrap();
        assert!(all.all_recipients());
        assert_eq!(all.activated_at, "2026-10-05T03:00:00+00:00");
        let some = ActivationGuard::parse(
            Some("2026-10-05T03:00:00Z"),
            Some(" Finance@Example.test , ,parent@example.test"),
        )
        .unwrap();
        assert!(!some.all_recipients());
        assert_eq!(
            some.recipient_list(),
            vec!["finance@example.test", "parent@example.test"]
        );
    }
}
