//! Settings an administrator sets for every user of a computer: read from
//! `HKLM\SOFTWARE\Policies\TideDesk`, where Group Policy, device management
//! (Intune and the like) or a `.reg` file put them. Only an administrator can
//! write there, so the person using the computer cannot change them, and
//! TideDesk shows them as "Set by your organisation". A program built on
//! TideDesk can add its own through [`set_provider`]; what the registry says
//! wins.
//!
//! | Value | Type | Meaning |
//! |---|---|---|
//! | `Rendezvous` | DWORD | 0: no connection service at all |
//! | `RendezvousServer` | SZ | only this connection service |
//! | `RendezvousAllowed` | MULTI_SZ | only these connection services |
//! | `LanDiscovery` | DWORD | 0/1: found by device ID on the local network |
//! | `TypedAddresses` | DWORD | 0/1: connect to typed addresses, not only device IDs |
//! | `AccessCode` | DWORD | 0/1: viewers may come in with the access code |
//! | `SavedPassword` | DWORD | 0/1: viewers may come in with the saved password |
//! | `PortMapping` | DWORD | 0/1: ask the router to open a port |
//! | `AllowStopSharing` | DWORD | 0/1: the person at the computer may stop sharing or quit |

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tidedesk_rendezvous_proto::DEFAULT_PORT;

/// Where the settings are, under `HKEY_LOCAL_MACHINE`.
pub const KEY: &str = r"SOFTWARE\Policies\TideDesk";

/// What a setting someone may not change says next to it.
pub const LOCKED_NOTE: &str = "Set by your organisation.";

/// Which connection services may be used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Services {
    /// Any: what the person chose.
    #[default]
    Any,
    /// Only this one, whatever was chosen; it is always used.
    Only(String),
    /// One of these; the first when the person chose another.
    OneOf(Vec<String>),
    /// None at all: no device-ID connections over the internet.
    Off,
}

impl Services {
    /// The service to use given what the person chose (`None`: none), and
    /// whether that differs from the choice.
    pub fn choose(&self, chosen: Option<&str>) -> Choice {
        let chosen = chosen.map(str::trim).filter(|s| !s.is_empty());
        match self {
            Services::Any => Choice::Chosen(chosen.map(str::to_string)),
            Services::Off => match chosen {
                None => Choice::Chosen(None),
                Some(_) => Choice::Overruled(None),
            },
            Services::Only(only) => {
                if chosen.is_some_and(|c| same_service(c, only)) {
                    Choice::Chosen(Some(only.clone()))
                } else {
                    Choice::Overruled(Some(only.clone()))
                }
            }
            Services::OneOf(list) => match chosen {
                Some(c) if list.iter().any(|s| same_service(c, s)) => {
                    Choice::Chosen(Some(c.to_string()))
                }
                _ => Choice::Overruled(list.first().cloned()),
            },
        }
    }

    /// Whether the person may pick a service or turn it off.
    pub fn locked(&self) -> bool {
        *self != Services::Any
    }
}

/// What [`Services::choose`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// What was chosen, allowed.
    Chosen(Option<String>),
    /// Not what was chosen: the administrator's.
    Overruled(Option<String>),
}

impl Choice {
    pub fn service(&self) -> Option<&str> {
        match self {
            Choice::Chosen(s) | Choice::Overruled(s) => s.as_deref(),
        }
    }
}

/// Whether two names are the same service: case aside, with the usual port
/// when none is given.
pub fn same_service(a: &str, b: &str) -> bool {
    let full = |s: &str| crate::net::with_default_port(s.trim(), DEFAULT_PORT).to_lowercase();
    full(a) == full(b)
}

/// The settings an administrator set; `None` where they set nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub services: Services,
    pub lan_discovery: Option<bool>,
    pub typed_addresses: Option<bool>,
    pub access_code: Option<bool>,
    pub saved_password: Option<bool>,
    pub port_mapping: Option<bool>,
    pub stop_sharing: Option<bool>,
}

impl Policy {
    /// `self`, with what it leaves unset taken from `under`.
    pub fn over(self, under: Policy) -> Policy {
        Policy {
            services: if self.services.locked() {
                self.services
            } else {
                under.services
            },
            lan_discovery: self.lan_discovery.or(under.lan_discovery),
            typed_addresses: self.typed_addresses.or(under.typed_addresses),
            access_code: self.access_code.or(under.access_code),
            saved_password: self.saved_password.or(under.saved_password),
            port_mapping: self.port_mapping.or(under.port_mapping),
            stop_sharing: self.stop_sharing.or(under.stop_sharing),
        }
    }

    /// Whether anything is set: TideDesk then says the computer is managed.
    pub fn any(&self) -> bool {
        *self != Policy::default()
    }

    /// `setting` as the person chose it, unless the administrator set it.
    pub fn bool_or(set: Option<bool>, chosen: bool) -> bool {
        set.unwrap_or(chosen)
    }
}

/// A value as the registry holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Dword(u32),
    Text(String),
    List(Vec<String>),
}

/// The settings from named values, as [`registry`] reads them. Values of
/// the wrong kind or empty are ignored, with a warning.
pub fn parse(get: impl Fn(&str) -> Option<Value>) -> Policy {
    let flag = |name: &str| match get(name) {
        Some(Value::Dword(n)) => Some(n != 0),
        None => None,
        Some(other) => {
            tracing::warn!("administrator setting {name} ignored: {other:?} is not 0 or 1");
            None
        }
    };
    let text = |name: &str| match get(name) {
        Some(Value::Text(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        None => None,
        Some(other) => {
            tracing::warn!("administrator setting {name} ignored: {other:?}");
            None
        }
    };
    let list = |name: &str| match get(name) {
        Some(Value::List(items)) => {
            let items: Vec<String> = items
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            (!items.is_empty()).then_some(items)
        }
        // One service typed as text instead of a list.
        Some(Value::Text(s)) if !s.trim().is_empty() => Some(vec![s.trim().to_string()]),
        None => None,
        Some(other) => {
            tracing::warn!("administrator setting {name} ignored: {other:?}");
            None
        }
    };
    let services = if flag("Rendezvous") == Some(false) {
        Services::Off
    } else if let Some(only) = text("RendezvousServer") {
        Services::Only(only)
    } else if let Some(list) = list("RendezvousAllowed") {
        Services::OneOf(list)
    } else {
        Services::Any
    };
    Policy {
        services,
        lan_discovery: flag("LanDiscovery"),
        typed_addresses: flag("TypedAddresses"),
        access_code: flag("AccessCode"),
        saved_password: flag("SavedPassword"),
        port_mapping: flag("PortMapping"),
        stop_sharing: flag("AllowStopSharing"),
    }
}

/// The settings in `HKLM\SOFTWARE\Policies\TideDesk`.
pub fn registry() -> Policy {
    #[cfg(windows)]
    {
        parse(|name| {
            read_value(
                windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
                KEY,
                name,
            )
        })
    }
    #[cfg(not(windows))]
    {
        Policy::default()
    }
}

/// One value under `root\path`, if it is there and of a kind TideDesk reads.
#[cfg(windows)]
fn read_value(
    root: windows::Win32::System::Registry::HKEY,
    path: &str,
    name: &str,
) -> Option<Value> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        REG_DWORD, REG_MULTI_SZ, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_DWORD, RRF_RT_REG_MULTI_SZ,
        RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::HSTRING;

    let (path, name) = (HSTRING::from(path), HSTRING::from(name));
    let flags = RRF_RT_REG_DWORD | RRF_RT_REG_SZ | RRF_RT_REG_MULTI_SZ;
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    // SAFETY: the first call only reports the size; the second writes at
    // most `size` bytes into a buffer of that size.
    let data = unsafe {
        if RegGetValueW(
            root,
            &path,
            &name,
            flags,
            Some(&mut kind),
            None,
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if RegGetValueW(
            root,
            &path,
            &name,
            flags,
            Some(&mut kind),
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        data.truncate(size as usize);
        data
    };
    let text = || {
        let wide: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        String::from_utf16_lossy(&wide)
    };
    match kind {
        REG_DWORD => Some(Value::Dword(u32::from_le_bytes(
            data.get(..4)?.try_into().ok()?,
        ))),
        REG_SZ => Some(Value::Text(text().trim_end_matches('\0').to_string())),
        REG_MULTI_SZ => Some(Value::List(
            text()
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        )),
        _ => None,
    }
}

/// Settings from a program built on TideDesk, used where the registry sets
/// nothing.
pub trait Provider: Send + Sync {
    fn policy(&self) -> Policy;
}

static PROVIDER: RwLock<Option<Arc<dyn Provider>>> = RwLock::new(None);

/// Sets the program's settings provider, replacing any earlier one.
pub fn set_provider(provider: Arc<dyn Provider>) {
    *PROVIDER.write().unwrap() = Some(provider);
    *CACHE.lock().unwrap() = None;
}

static CACHE: Mutex<Option<(Instant, Policy)>> = Mutex::new(None);

/// The settings in force: the registry's, over the provider's. Read again
/// every few seconds, not on every frame.
pub fn current() -> Policy {
    let mut cache = CACHE.lock().unwrap();
    if let Some((at, policy)) = &*cache
        && at.elapsed() < Duration::from_secs(5)
    {
        return policy.clone();
    }
    let provided = PROVIDER
        .read()
        .unwrap()
        .as_ref()
        .map(|p| p.policy())
        .unwrap_or_default();
    let policy = registry().over(provided);
    *cache = Some((Instant::now(), policy.clone()));
    policy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nat::signal::DEFAULT_RENDEZVOUS;

    fn values(pairs: &[(&str, Value)]) -> impl Fn(&str) -> Option<Value> {
        let pairs: Vec<(String, Value)> = pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn nothing_set_changes_nothing() {
        let policy = parse(|_| None);
        assert_eq!(policy, Policy::default());
        assert!(!policy.any());
        assert_eq!(
            policy.services.choose(Some("rv.example:47900")),
            Choice::Chosen(Some("rv.example:47900".into()))
        );
        assert_eq!(policy.services.choose(None), Choice::Chosen(None));
    }

    #[test]
    fn flags_are_read_and_bad_values_ignored() {
        let policy = parse(values(&[
            ("LanDiscovery", Value::Dword(0)),
            ("TypedAddresses", Value::Dword(1)),
            ("AccessCode", Value::Dword(7)),
            ("SavedPassword", Value::Text("no".into())),
            ("AllowStopSharing", Value::Dword(0)),
        ]));
        assert_eq!(policy.lan_discovery, Some(false));
        assert_eq!(policy.typed_addresses, Some(true));
        assert_eq!(policy.access_code, Some(true), "any non-zero is on");
        assert_eq!(policy.saved_password, None, "text where 0/1 belongs");
        assert_eq!(policy.stop_sharing, Some(false));
        assert_eq!(policy.port_mapping, None);
        assert!(policy.any());
    }

    #[test]
    fn one_service_is_always_used() {
        let policy = parse(values(&[(
            "RendezvousServer",
            Value::Text(" tidedesk.example.com ".into()),
        )]));
        let only = Services::Only("tidedesk.example.com".into());
        assert_eq!(policy.services, only);
        assert!(only.locked());
        assert_eq!(
            only.choose(Some("TideDesk.Example.com:47900")),
            Choice::Chosen(Some("tidedesk.example.com".into())),
            "the same service, named differently"
        );
        for chosen in [None, Some(DEFAULT_RENDEZVOUS), Some("other.example:47900")] {
            assert_eq!(
                only.choose(chosen),
                Choice::Overruled(Some("tidedesk.example.com".into())),
                "{chosen:?}"
            );
        }
    }

    #[test]
    fn a_list_of_services_allows_one_of_them() {
        let policy = parse(values(&[(
            "RendezvousAllowed",
            Value::List(vec![
                "a.example".into(),
                " ".into(),
                "b.example:48000".into(),
            ]),
        )]));
        let list = Services::OneOf(vec!["a.example".into(), "b.example:48000".into()]);
        assert_eq!(policy.services, list);
        assert_eq!(
            list.choose(Some("b.example:48000")),
            Choice::Chosen(Some("b.example:48000".into()))
        );
        assert_eq!(
            list.choose(Some("b.example")),
            Choice::Overruled(Some("a.example".into())),
            "another port is another service"
        );
        assert_eq!(
            list.choose(None),
            Choice::Overruled(Some("a.example".into()))
        );
        let typed_as_text = parse(values(&[(
            "RendezvousAllowed",
            Value::Text("a.example".into()),
        )]));
        assert_eq!(
            typed_as_text.services,
            Services::OneOf(vec!["a.example".into()])
        );
    }

    #[test]
    fn no_service_beats_the_others() {
        let policy = parse(values(&[
            ("Rendezvous", Value::Dword(0)),
            ("RendezvousServer", Value::Text("a.example".into())),
        ]));
        assert_eq!(policy.services, Services::Off);
        assert_eq!(
            policy.services.choose(Some("a.example")),
            Choice::Overruled(None)
        );
        assert_eq!(policy.services.choose(None), Choice::Chosen(None));
        let on = parse(values(&[("Rendezvous", Value::Dword(1))]));
        assert_eq!(on.services, Services::Any, "1 adds nothing");
    }

    #[test]
    fn the_registry_wins_over_a_program_and_fills_in_from_it() {
        let registry = Policy {
            lan_discovery: Some(false),
            ..Policy::default()
        };
        let program = Policy {
            services: Services::Only("a.example".into()),
            lan_discovery: Some(true),
            stop_sharing: Some(false),
            ..Policy::default()
        };
        let merged = registry.clone().over(program.clone());
        assert_eq!(merged.lan_discovery, Some(false), "the registry's");
        assert_eq!(
            merged.services,
            Services::Only("a.example".into()),
            "the program's"
        );
        assert_eq!(merged.stop_sharing, Some(false));
        let locked = Policy {
            services: Services::Off,
            ..Policy::default()
        };
        assert_eq!(locked.over(program).services, Services::Off);
    }

    /// Writes values under the current user's key, which needs no
    /// administrator, and reads them back the way the policy key is read.
    #[cfg(windows)]
    #[test]
    fn values_are_read_from_the_registry() {
        use windows::Win32::System::Registry::{
            HKEY_CURRENT_USER, REG_DWORD, REG_MULTI_SZ, REG_SZ, RegDeleteTreeW, RegSetKeyValueW,
        };
        use windows::core::HSTRING;
        let path = format!(r"Software\TideDesk-test-policy-{}", std::process::id());
        let key = HSTRING::from(path.as_str());
        let wide = |s: &str| -> Vec<u8> {
            s.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        };
        let one = 1u32.to_le_bytes();
        let text = wide("tidedesk.example.com");
        let list: Vec<u8> = "a.example\0b.example\0\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: each buffer outlives its call and its length is passed.
        unsafe {
            for (name, kind, data) in [
                ("LanDiscovery", REG_DWORD, &one[..]),
                ("RendezvousServer", REG_SZ, &text[..]),
                ("RendezvousAllowed", REG_MULTI_SZ, &list[..]),
            ] {
                let status = RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    &key,
                    &HSTRING::from(name),
                    kind.0,
                    Some(data.as_ptr().cast()),
                    data.len() as u32,
                );
                assert!(status.is_ok(), "{name}: {status:?}");
            }
        }
        let read = |name: &str| read_value(HKEY_CURRENT_USER, &path, name);
        assert_eq!(read("LanDiscovery"), Some(Value::Dword(1)));
        assert_eq!(
            read("RendezvousServer"),
            Some(Value::Text("tidedesk.example.com".into()))
        );
        assert_eq!(
            read("RendezvousAllowed"),
            Some(Value::List(vec!["a.example".into(), "b.example".into()]))
        );
        assert_eq!(read("Missing"), None);
        let policy = parse(read);
        assert_eq!(
            policy.services,
            Services::Only("tidedesk.example.com".into())
        );
        // SAFETY: deletes only the test's own key.
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &key);
        }
    }
}
