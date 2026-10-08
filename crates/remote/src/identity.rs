//! The identity chain (§10.1): each host shows the hops that reached it. A hop is a principal,
//! a method (`local`, `ssh-key`, or `tailscale` later), the machine it came from, and the user it
//! acts as on the host, as in "can@github via tailscale from laptop as can@build-box".

use std::fmt;

use serde::{Deserialize, Serialize};

/// The method of a hop through an SSH public key.
pub const SSH_KEY: &str = "ssh-key";
/// The method of the local hop: a window on the daemon's own machine.
pub const LOCAL: &str = "local";

/// One hop of an identity chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    /// Who authenticated: for an SSH key, its comment, else its fingerprint.
    pub principal: String,
    /// How: `local` or `ssh-key` (another SSH method shows as `ssh-<method>`).
    pub method: String,
    /// The machine the hop started from.
    pub origin: String,
    /// The user it acts as on the host, `user@host`.
    pub acting: String,
    /// The key's type and fingerprint, as `ED25519 SHA256:…`, for a key hop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl Hop {
    /// The hop of a window on this machine.
    pub fn local() -> Hop {
        let me = format!(
            "{}@{}",
            crate::bridge::current_user(),
            crate::bridge::hostname()
        );
        Hop {
            principal: me.clone(),
            method: LOCAL.to_owned(),
            origin: crate::bridge::hostname(),
            acting: me,
            key: None,
        }
    }
}

impl From<Hop> for auk_mirror::Hop {
    fn from(h: Hop) -> Self {
        auk_mirror::Hop {
            principal: h.principal,
            method: h.method,
            origin: h.origin,
            acting: h.acting,
            key: h.key,
        }
    }
}

impl fmt::Display for Hop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} via {} from {} as {}",
            self.principal, self.method, self.origin, self.acting
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_like_terns_chain() {
        let hop = Hop {
            principal: "can@github".into(),
            method: "tailscale".into(),
            origin: "laptop".into(),
            acting: "can@build-box".into(),
            key: None,
        };
        assert_eq!(
            hop.to_string(),
            "can@github via tailscale from laptop as can@build-box"
        );
        let local = Hop::local();
        assert_eq!(local.method, LOCAL);
        assert!(local.acting.ends_with(&local.origin));
    }
}
