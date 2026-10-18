//! Persisted request boundary for unattended quota spending. The watcher is a
//! UI/lifecycle convenience, not the authority for which account may be billed.
use super::{accounts, burner, providers::Target, store::Settings, Harness};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurnPolicy {
    pub account_id: String,
    pub model: String,
    pub window: String,
    pub resets_at: i64,
    /// Immutable ceiling selected when the item started. Lowering the global
    /// setting takes effect too, but raising it cannot enlarge an existing run.
    pub spend_cap: f64,
}

impl BurnPolicy {
    /// Re-evaluated immediately before every outbound attempt, not just when
    /// the router builds a list (which can outlive refreshes, retries or pauses).
    pub fn check(&self, h: &Harness, s: &Settings, target: &Target) -> Result<(), String> {
        let now = accounts::now();
        let fail = |reason: &str| Err(format!("Quota burner: {reason}"));
        if !s.burner.enabled {
            return fail("switched off");
        }
        if self.resets_at <= now {
            return fail("window reset");
        }
        if target.model_id != self.model
            || target.account.as_ref().map(|a| a.id.as_str()) != Some(self.account_id.as_str())
        {
            return fail("request does not match its pinned model/account");
        }
        let Some(a) = s.accounts.iter().find(|a| a.id == self.account_id) else {
            return fail("pinned account was removed");
        };
        let Some(u) = h.accts.usage_of(&a.id) else {
            return fail("usage reading unavailable; refusing unmeasured spend");
        };
        if !u.error.is_empty()
            || u.fetched <= 0
            || u.fetched > now + 120
            || now - u.fetched > burner::STALE_SECS
        {
            return fail("usage reading stale or unavailable; refusing unmeasured spend");
        }
        let Some(window) = u.windows.iter().find(|w| w.label == self.window) else {
            return fail("pinned window reading unavailable");
        };
        if window.resets_at == 0 {
            return fail("window reset time unavailable");
        }
        if window.resets_at > self.resets_at + 120 {
            return fail("window reset");
        }
        let limit = self.spend_cap.min(s.burner.spend_cap);
        if !limit.is_finite() || !u.windows.iter().all(|w| w.used.is_finite()) {
            return fail("usage reading or spend cap invalid");
        }
        if u.windows
            .iter()
            .any(|w| w.used >= burner::ceiling(&w.label, &a.caps, limit, now))
        {
            return fail("spend cap or account usage cap reached");
        }
        // Do not use usable_for_model: free-model exceptions deliberately skip
        // paid caps. Unattended spending must always respect the standing caps.
        if !h.accts.usable(a) {
            return fail("pinned account unavailable (limit, cap, disabled or cooldown)");
        }
        Ok(())
    }
}
