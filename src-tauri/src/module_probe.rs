use crate::{chatgpt_client::ChatGptPlan, cookie_artifact::PreparedCookieArtifact};
use std::time::Duration;

pub(crate) trait ProbeControl: Send + Sync {
    fn is_cancelled(&self) -> bool;
    fn wait_cancelled(&self, duration: Duration) -> bool;
}

pub(crate) trait CookieModuleProber: Send + Sync + 'static {
    fn check(
        &self,
        artifact: &PreparedCookieArtifact,
        control: &dyn ProbeControl,
    ) -> ModuleProbeResult;
}

/// A module-neutral result used by the task engine.  It intentionally carries only
/// bounded classification data: account identifiers and authentication material never
/// cross into task snapshots, history, or frontend events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModuleProbeResult {
    pub(crate) status: ModuleProbeStatus,
    pub(crate) retries: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModuleProbeStatus {
    Active(ModulePlan),
    Dead,
    RateLimited,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModulePlan {
    ChatGpt(ChatGptPlan),
    Twitch(TwitchPlan),
    Max(MaxPlan),
}

impl ModulePlan {
    pub(crate) fn label(self) -> String {
        match self {
            Self::ChatGpt(plan) => chatgpt_plan_label(plan).to_string(),
            Self::Twitch(plan) => plan.label(),
            Self::Max(plan) => plan.label(),
        }
    }

    pub(crate) fn slug(self) -> String {
        match self {
            Self::ChatGpt(plan) => chatgpt_plan_slug(plan).to_string(),
            Self::Twitch(plan) => plan.slug(),
            Self::Max(plan) => plan.slug(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MaxTier {
    BasicWithAds,
    Mobile,
    Standard,
    AdFree,
    Premium,
    Platinum,
    Legacy,
    #[default]
    Unknown,
}

impl MaxTier {
    fn label(self) -> &'static str {
        match self {
            Self::BasicWithAds => "Basic with Ads",
            Self::Mobile => "Mobile",
            Self::Standard => "Standard",
            Self::AdFree => "Ad-Free",
            Self::Premium => "Premium",
            Self::Platinum => "Platinum",
            Self::Legacy => "Legacy",
            Self::Unknown => "Unknown plan",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::BasicWithAds => "basic-with-ads",
            Self::Mobile => "mobile",
            Self::Standard => "standard",
            Self::AdFree => "ad-free",
            Self::Premium => "premium",
            Self::Platinum => "platinum",
            Self::Legacy => "legacy",
            Self::Unknown => "unknown-plan",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MaxSubscriptionState {
    Active,
    InGracePeriod,
    PreActive,
    Paused,
    Cancelled,
    Expired,
    NoSubscription,
    #[default]
    Unknown,
}

impl MaxSubscriptionState {
    fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::InGracePeriod => "In grace period",
            Self::PreActive => "Pre-active",
            Self::Paused => "Paused",
            Self::Cancelled => "Cancelled",
            Self::Expired => "Expired",
            Self::NoSubscription => "No subscription",
            Self::Unknown => "Unknown status",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::InGracePeriod => "grace",
            Self::PreActive => "pre-active",
            Self::Paused => "paused",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::NoSubscription => "no-subscription",
            Self::Unknown => "unknown-status",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MaxPlan {
    pub(crate) tier: MaxTier,
    pub(crate) state: MaxSubscriptionState,
}

impl MaxPlan {
    pub(crate) fn label(self) -> String {
        if self.state == MaxSubscriptionState::NoSubscription {
            return self.state.label().to_string();
        }
        format!("{} ({})", self.tier.label(), self.state.label())
    }

    pub(crate) fn slug(self) -> String {
        if self.state == MaxSubscriptionState::NoSubscription {
            return self.state.slug().to_string();
        }
        format!("{}-{}", self.tier.slug(), self.state.slug())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TwitchRole {
    #[default]
    Viewer,
    Affiliate,
    Partner,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TwitchPlan {
    pub(crate) has_prime: bool,
    pub(crate) has_turbo: bool,
    pub(crate) role: TwitchRole,
}

impl TwitchPlan {
    pub(crate) fn label(self) -> String {
        self.parts(" + ", "Standard")
    }

    pub(crate) fn slug(self) -> String {
        self.parts("-", "standard").to_ascii_lowercase()
    }

    fn parts(self, separator: &str, fallback: &str) -> String {
        let mut parts = Vec::with_capacity(3);
        if self.has_prime {
            parts.push("Prime");
        }
        if self.has_turbo {
            parts.push("Turbo");
        }
        match self.role {
            TwitchRole::Viewer => {}
            TwitchRole::Affiliate => parts.push("Affiliate"),
            TwitchRole::Partner => parts.push("Partner"),
        }
        if parts.is_empty() {
            fallback.to_string()
        } else {
            parts.join(separator)
        }
    }
}

fn chatgpt_plan_label(plan: ChatGptPlan) -> &'static str {
    match plan {
        ChatGptPlan::Free => "Free",
        ChatGptPlan::Go => "Go",
        ChatGptPlan::Plus => "Plus",
        ChatGptPlan::Pro => "Pro",
        ChatGptPlan::Team => "Team",
        ChatGptPlan::Enterprise => "Enterprise",
    }
}

fn chatgpt_plan_slug(plan: ChatGptPlan) -> &'static str {
    match plan {
        ChatGptPlan::Free => "free",
        ChatGptPlan::Go => "go",
        ChatGptPlan::Plus => "plus",
        ChatGptPlan::Pro => "pro",
        ChatGptPlan::Team => "team",
        ChatGptPlan::Enterprise => "enterprise",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twitch_plan_is_stable_for_summary_and_file_names() {
        let plan = TwitchPlan {
            has_prime: true,
            has_turbo: true,
            role: TwitchRole::Partner,
        };

        assert_eq!(plan.label(), "Prime + Turbo + Partner");
        assert_eq!(plan.slug(), "prime-turbo-partner");
        assert_eq!(TwitchPlan::default().label(), "Standard");
        assert_eq!(TwitchPlan::default().slug(), "standard");
    }

    #[test]
    fn chatgpt_plan_names_remain_compatible_with_existing_exports() {
        let plan = ModulePlan::ChatGpt(ChatGptPlan::Plus);
        assert_eq!(plan.label(), "Plus");
        assert_eq!(plan.slug(), "plus");
    }

    #[test]
    fn max_plan_names_include_subscription_state_without_sensitive_data() {
        let plan = ModulePlan::Max(MaxPlan {
            tier: MaxTier::Premium,
            state: MaxSubscriptionState::InGracePeriod,
        });
        assert_eq!(plan.label(), "Premium (In grace period)");
        assert_eq!(plan.slug(), "premium-grace");

        let no_subscription = MaxPlan {
            tier: MaxTier::Unknown,
            state: MaxSubscriptionState::NoSubscription,
        };
        assert_eq!(no_subscription.label(), "No subscription");
        assert_eq!(no_subscription.slug(), "no-subscription");
    }
}
