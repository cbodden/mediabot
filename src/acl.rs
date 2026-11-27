//! Ownership / access control and the persisted runtime state (users + channels).
//!
//! Identity is the NickServ account delivered by the IRCv3 `account-tag` capability, which
//! cannot be spoofed by changing nick. Entries that contain `!` or `@` are instead treated as
//! `nick!user@host` glob masks for people without an account.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::ChannelConfig;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default, Hash,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    None,
    #[serde(alias = "ro", alias = "read", alias = "read-only", alias = "read_only")]
    ReadOnly,
    Admin,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::None => "none",
            Role::ReadOnly => "readonly",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s.to_ascii_lowercase().as_str() {
            "ro" | "read" | "readonly" | "read-only" | "user" => Some(Role::ReadOnly),
            "admin" | "rw" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// Who sent a message.
#[derive(Debug, Clone, Default)]
pub struct Sender {
    pub nick: String,
    pub user: String,
    pub host: String,
    pub account: Option<String>,
}

impl Sender {
    pub fn mask(&self) -> String {
        format!("{}!{}@{}", self.nick, self.user, self.host)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserEntry {
    /// NickServ account name, or a `nick!user@host` glob.
    pub id: String,
    pub role: Role,
    #[serde(default)]
    pub added_by: String,
    pub added_at: DateTime<Utc>,
}

impl UserEntry {
    pub fn is_mask(&self) -> bool {
        is_mask(&self.id)
    }
    fn matches(&self, s: &Sender) -> bool {
        if self.is_mask() {
            glob_match(&irc_lower(&self.id), &irc_lower(&s.mask()))
        } else {
            s.account
                .as_deref()
                .is_some_and(|a| irc_lower(a) == irc_lower(&self.id))
        }
    }
}

pub fn is_mask(id: &str) -> bool {
    id.contains('!') || id.contains('@')
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    users: Vec<UserEntry>,
    /// Channels the bot should be in. `None` until changed at runtime, so config is used.
    #[serde(default)]
    channels: Option<Vec<ChannelConfig>>,
}

pub struct Store {
    path: PathBuf,
    owners: Vec<String>,
    state: RwLock<PersistedState>,
}

impl Store {
    pub fn open(data_dir: &Path, owners: &[String]) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let path = data_dir.join("state.json");
        let state = match std::fs::read_to_string(&path) {
            Ok(s) => {
                serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PersistedState::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path,
            owners: owners.iter().map(|o| irc_lower(o)).collect(),
            state: RwLock::new(state),
        })
    }

    async fn save(&self, st: &PersistedState) -> Result<()> {
        let json = serde_json::to_vec_pretty(st)?;
        let tmp = self.path.with_extension("json.tmp");
        tokio::fs::write(&tmp, json).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    pub fn owners(&self) -> &[String] {
        &self.owners
    }

    /// Effective role of `sender`, given the fallback role for the context (channel or PM).
    pub async fn role_of(&self, sender: &Sender, fallback: Role) -> Role {
        if let Some(acct) = &sender.account
            && self.owners.contains(&irc_lower(acct))
        {
            return Role::Owner;
        }
        let st = self.state.read().await;
        st.users
            .iter()
            .filter(|u| u.matches(sender))
            .map(|u| u.role)
            .max()
            .unwrap_or(Role::None)
            .max(fallback)
    }

    pub async fn users(&self) -> Vec<UserEntry> {
        let mut v = self.state.read().await.users.clone();
        v.sort_by(|a, b| b.role.cmp(&a.role).then(a.id.cmp(&b.id)));
        v
    }

    pub async fn user_counts(&self) -> [(Role, usize); 3] {
        let st = self.state.read().await;
        let n = |r| st.users.iter().filter(|u| u.role == r).count();
        [
            (Role::Owner, self.owners.len()),
            (Role::Admin, n(Role::Admin)),
            (Role::ReadOnly, n(Role::ReadOnly)),
        ]
    }

    /// Add or change a user. `actor_role` enforces the delegation rules:
    /// owners manage everyone; admins may only manage read-only entries.
    pub async fn set_user(
        &self,
        id: &str,
        role: Role,
        actor: &str,
        actor_role: Role,
    ) -> Result<bool> {
        let id = id.trim();
        if id.is_empty() || id.contains(char::is_whitespace) {
            bail!("invalid user id");
        }
        if !matches!(role, Role::ReadOnly | Role::Admin) {
            bail!("role must be readonly or admin (owners are set in the config file)");
        }
        if !is_mask(id) && self.owners.contains(&irc_lower(id)) {
            bail!("{id} is an owner (from config); nothing to change");
        }
        let mut st = self.state.write().await;
        let existing = st
            .users
            .iter()
            .position(|u| irc_lower(&u.id) == irc_lower(id));
        if actor_role < Role::Owner {
            if role > Role::ReadOnly {
                bail!("only owners can grant admin");
            }
            if let Some(i) = existing
                && st.users[i].role > Role::ReadOnly
            {
                bail!("only owners can change an admin");
            }
        }
        let entry = UserEntry {
            id: id.to_string(),
            role,
            added_by: actor.to_string(),
            added_at: Utc::now(),
        };
        let created = match existing {
            Some(i) => {
                st.users[i] = entry;
                false
            }
            None => {
                st.users.push(entry);
                true
            }
        };
        self.save(&st).await?;
        Ok(created)
    }

    pub async fn remove_user(&self, id: &str, actor_role: Role) -> Result<()> {
        let mut st = self.state.write().await;
        let Some(i) = st
            .users
            .iter()
            .position(|u| irc_lower(&u.id) == irc_lower(id))
        else {
            if self.owners.contains(&irc_lower(id)) {
                bail!("{id} is an owner from the config file and can only be removed there");
            }
            bail!("no such user {id}");
        };
        if actor_role < Role::Owner && st.users[i].role > Role::ReadOnly {
            bail!("only owners can remove an admin");
        }
        st.users.remove(i);
        self.save(&st).await
    }

    pub async fn channels(&self, config_channels: &[ChannelConfig]) -> Vec<ChannelConfig> {
        self.state
            .read()
            .await
            .channels
            .clone()
            .unwrap_or_else(|| config_channels.to_vec())
    }

    pub async fn set_channels(&self, chans: Vec<ChannelConfig>) -> Result<()> {
        let mut st = self.state.write().await;
        st.channels = Some(chans);
        self.save(&st).await
    }
}

/// RFC1459 casemapping (what Libera uses).
pub fn irc_lower(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '[' => '{',
            ']' => '}',
            '\\' => '|',
            '~' => '^',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// `*` and `?` glob match.
pub fn glob_match(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender(nick: &str, acct: Option<&str>) -> Sender {
        Sender {
            nick: nick.into(),
            user: "u".into(),
            host: "user/x".into(),
            account: acct.map(Into::into),
        }
    }

    #[test]
    fn globbing() {
        assert!(glob_match("*!*@user/x", "nick!u@user/x"));
        assert!(glob_match("n?ck!*", "nick!u@h"));
        assert!(!glob_match("*!*@other", "nick!u@user/x"));
    }

    #[tokio::test]
    async fn roles_and_delegation() {
        let dir = std::env::temp_dir().join(format!("mediabot-acl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir, &["Boss".into()]).unwrap();
        assert_eq!(
            store.role_of(&sender("x", Some("boss")), Role::None).await,
            Role::Owner
        );
        assert_eq!(
            store.role_of(&sender("x", None), Role::ReadOnly).await,
            Role::ReadOnly
        );

        store
            .set_user("alice", Role::Admin, "boss", Role::Owner)
            .await
            .unwrap();
        assert_eq!(
            store
                .role_of(&sender("whatever", Some("Alice")), Role::None)
                .await,
            Role::Admin
        );
        // nick alone (no account) must not match an account entry
        assert_eq!(
            store.role_of(&sender("alice", None), Role::None).await,
            Role::None
        );

        // admins can add read-only but not admins, and can't demote admins
        store
            .set_user("bob", Role::ReadOnly, "alice", Role::Admin)
            .await
            .unwrap();
        assert!(
            store
                .set_user("carol", Role::Admin, "alice", Role::Admin)
                .await
                .is_err()
        );
        assert!(
            store
                .set_user("alice", Role::ReadOnly, "bob", Role::Admin)
                .await
                .is_err()
        );
        assert!(store.remove_user("alice", Role::Admin).await.is_err());
        assert!(store.remove_user("boss", Role::Owner).await.is_err());

        store
            .set_user("*!*@user/x", Role::ReadOnly, "boss", Role::Owner)
            .await
            .unwrap();
        assert_eq!(
            store.role_of(&sender("anyone", None), Role::None).await,
            Role::ReadOnly
        );

        // persisted
        drop(store);
        let store = Store::open(&dir, &[]).unwrap();
        assert_eq!(store.users().await.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
