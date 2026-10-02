//! Company computers: one published rule instead of guessing from how
//! TideDesk is used. A computer managed by an organisation needs a licence;
//! without one it has a 14-day trial, then a few hours a month. Whether a
//! computer is managed is read from Windows' own records on the computer
//! itself, and nothing about it is sent anywhere.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Days a company computer works without limits before its monthly hours apply.
pub const TRIAL_DAYS: i64 = 14;
/// Hours a month that work as usual.
pub const CLEAN: Duration = Duration::from_secs(2 * 3600);
/// Hours a month in all; past [`CLEAN`] the viewer shows a mark.
pub const LIMIT: Duration = Duration::from_secs(8 * 3600);
/// How long before the end a running session is warned.
pub const WARNING: Duration = Duration::from_secs(10 * 60);

/// How an organisation manages this computer, if it does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Management {
    /// The Active Directory domain it is joined to.
    pub domain: Option<String>,
    /// The Microsoft Entra ID organisation it is joined to (a device join,
    /// not a personal computer that only added a work account).
    pub entra: Option<String>,
    /// Enrolled in device management (Intune or similar).
    pub device_management: bool,
}

impl Management {
    pub fn managed(&self) -> bool {
        self.domain.is_some() || self.entra.is_some() || self.device_management
    }

    /// Only a plain domain may be declared personal (a home lab): real
    /// company computers are almost always joined to Entra ID or enrolled in
    /// device management as well.
    pub fn may_declare(&self) -> bool {
        self.domain.is_some() && self.entra.is_none() && !self.device_management
    }

    /// The organisation's name, for messages.
    pub fn name(&self) -> String {
        [&self.domain, &self.entra]
            .into_iter()
            .flatten()
            .find(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| "an organisation".into())
    }
}

/// This computer's management, read once. `TIDEDESK_PRETEND_MANAGED=NAME`
/// makes any computer count as joined to the domain NAME, for trying the
/// rule on a computer no organisation manages; it can only add the rule.
pub fn management() -> &'static Management {
    static READ: OnceLock<Management> = OnceLock::new();
    READ.get_or_init(|| {
        let mut management = read();
        if let Ok(name) = std::env::var("TIDEDESK_PRETEND_MANAGED")
            && !name.is_empty()
        {
            management.domain = Some(name);
        }
        management
    })
}

#[cfg(windows)]
fn read() -> Management {
    use windows::Win32::Management::MobileDeviceManagementRegistration::IsDeviceRegisteredWithManagement;
    use windows::Win32::NetworkManagement::NetManagement::{
        DSREG_DEVICE_JOIN, NETSETUP_JOIN_STATUS, NetApiBufferFree, NetFreeAadJoinInformation,
        NetGetAadJoinInformation, NetGetJoinInformation, NetSetupDomainName,
    };
    use windows::core::{BOOL, PCWSTR, PWSTR};

    let mut management = Management::default();
    // SAFETY: each buffer Windows returns is read before it is freed with
    // its own function, and only when the call succeeded.
    unsafe {
        let mut name = PWSTR::null();
        let mut status = NETSETUP_JOIN_STATUS::default();
        if NetGetJoinInformation(PCWSTR::null(), &mut name, &mut status) == 0 && !name.is_null() {
            if status == NetSetupDomainName {
                management.domain = name.to_string().ok();
            }
            NetApiBufferFree(Some(name.0 as _));
        }
        if let Ok(info) = NetGetAadJoinInformation(PCWSTR::null())
            && !info.is_null()
        {
            if (*info).joinType == DSREG_DEVICE_JOIN {
                let tenant = (*info).pszTenantDisplayName;
                management.entra = Some(if tenant.is_null() {
                    String::new()
                } else {
                    tenant.to_string().unwrap_or_default()
                });
            }
            NetFreeAadJoinInformation(Some(info));
        }
        let mut registered = BOOL(0);
        let mut upn = [0u16; 256];
        if IsDeviceRegisteredWithManagement(&mut registered, Some(&mut upn)).is_ok() {
            management.device_management = registered.as_bool();
        }
    }
    management
}

#[cfg(not(windows))]
fn read() -> Management {
    Management::default()
}

/// What someone confirms when declaring their domain computer personal.
pub const DECLARATION: &str = "This computer and its domain are mine, and I use TideDesk on it for \
personal, non-commercial purposes. A false declaration breaks the TideDesk license.";

/// A home lab's declaration that its domain computer is personal: who made
/// it, when, and for which domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub user: String,
    pub date: String,
    pub domain: String,
}

impl Declaration {
    /// Whether it still holds for a computer managed as `management`: made
    /// for the domain it is joined to, and nothing else manages it.
    pub fn holds(&self, management: &Management) -> bool {
        management.may_declare() && management.domain.as_deref() == Some(self.domain.as_str())
    }

    pub fn describe(&self) -> String {
        format!(
            "Declared personal by {} on {} (domain {}).",
            self.user, self.date, self.domain
        )
    }
}

fn declaration_path() -> Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("personal-declaration.toml"))
}

/// This computer's declaration, if it still holds.
pub fn declaration() -> Option<Declaration> {
    let text = std::fs::read_to_string(declaration_path().ok()?).ok()?;
    let declaration: Declaration = toml::from_str(&text).ok()?;
    declaration.holds(management()).then_some(declaration)
}

/// Declares this domain computer personal, by the Windows user signed in.
pub fn declare() -> Result<Declaration> {
    let management = management();
    let Some(domain) = management
        .domain
        .clone()
        .filter(|_| management.may_declare())
    else {
        anyhow::bail!(
            "only a computer on its own domain, without device management, can be declared personal"
        );
    };
    let declaration = Declaration {
        user: std::env::var("USERNAME").unwrap_or_else(|_| "unknown".into()),
        date: crate::dates::today(),
        domain,
    };
    let path = declaration_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("making the settings folder")?;
    }
    let text = toml::to_string(&declaration).context("writing the declaration")?;
    std::fs::write(&path, text).with_context(|| format!("saving {}", path.display()))?;
    Ok(declaration)
}

/// Withdraws this computer's declaration.
pub fn withdraw() -> Result<()> {
    match std::fs::remove_file(declaration_path()?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).context("removing the declaration")
        }
        _ => Ok(()),
    }
}

/// Where an unlicensed company computer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Allowance {
    /// The first days: no limits.
    Trial { days_left: u32 },
    /// Within the month's first hours: works as usual.
    Clear { minutes_left: u32 },
    /// Past them: works, and the viewer shows a mark.
    Marked { minutes_left: u32 },
    /// The month's hours are used: new sessions need a licence.
    Used,
}

impl Allowance {
    /// What the computer's own window says about it.
    pub fn describe(&self, organisation: &str) -> String {
        let start = format!("Company computer ({organisation}) without a TideDesk licence");
        match self {
            Allowance::Trial { days_left } => format!("{start}: trial, {days_left} days left."),
            Allowance::Clear { minutes_left } | Allowance::Marked { minutes_left } => format!(
                "{start}: {} left this month.",
                hours_and_minutes(*minutes_left)
            ),
            Allowance::Used => format!("{start}: this month's hours are used."),
        }
    }
}

/// "5 h 20 min", "40 min".
pub fn hours_and_minutes(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// What has been used on this computer, kept in the settings folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    /// The day this computer was first seen managed: the trial's start.
    pub first_seen: Option<String>,
    /// The month counted, `YYYY-MM`.
    pub month: String,
    /// Seconds of sessions in that month.
    pub seconds: u64,
}

impl Usage {
    /// Starts the trial on the first day seen.
    pub fn seen(&mut self, today: &str) {
        if self.first_seen.is_none() {
            self.first_seen = Some(today.to_string());
        }
    }

    fn trial_days_left(&self, today: &str) -> Option<u32> {
        let start = crate::dates::parse(self.first_seen.as_deref().unwrap_or(today))?;
        let used = crate::dates::parse(today)? - start;
        (used < TRIAL_DAYS).then(|| (TRIAL_DAYS - used.max(0)) as u32)
    }

    fn used_this_month(&self, today: &str) -> u64 {
        if self.month == today[..7.min(today.len())] {
            self.seconds
        } else {
            0
        }
    }

    pub fn allowance(&self, today: &str) -> Allowance {
        if let Some(days_left) = self.trial_days_left(today) {
            return Allowance::Trial { days_left };
        }
        let used = self.used_this_month(today);
        if used >= LIMIT.as_secs() {
            return Allowance::Used;
        }
        let minutes_left = (LIMIT.as_secs() - used).div_ceil(60) as u32;
        if used < CLEAN.as_secs() {
            Allowance::Clear { minutes_left }
        } else {
            Allowance::Marked { minutes_left }
        }
    }

    /// Counts `seconds` of a session; the trial's days are not counted.
    pub fn add(&mut self, today: &str, seconds: u64) {
        if self.trial_days_left(today).is_some() {
            return;
        }
        let month = &today[..7.min(today.len())];
        if self.month != month {
            self.month = month.to_string();
            self.seconds = 0;
        }
        self.seconds += seconds;
    }
}

/// `company-use.toml` in the settings folder.
pub fn path() -> Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("company-use.toml"))
}

pub fn load() -> Usage {
    path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(usage: &Usage) -> Result<()> {
    let path = path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("making the settings folder")?;
    }
    let text = toml::to_string(usage).context("writing the company hours")?;
    std::fs::write(&path, text).with_context(|| format!("saving {}", path.display()))
}

/// Where this computer stands today: `None` when no limit applies (not
/// managed, or licensed for work). Starts the trial on the first call.
pub fn allowance() -> Option<Allowance> {
    if !management().managed() || crate::licence::allows("work") || declaration().is_some() {
        return None;
    }
    let today = crate::dates::today();
    let mut usage = load();
    if usage.first_seen.is_none() {
        usage.seen(&today);
        if let Err(e) = save(&usage) {
            tracing::warn!("could not save the company hours: {e:#}");
        }
    }
    Some(usage.allowance(&today))
}

/// What a running session's meter has to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Nothing,
    /// Tell the viewer: the mark starts, the warning is due, or the trial ended.
    Tell(Allowance),
    /// The month's hours are used: end the session.
    Over,
}

/// Counts a session's time on an unlicensed company computer.
pub struct Meter {
    usage: Usage,
    told: Allowance,
    warned: bool,
    since: Instant,
    save: bool,
    today: fn() -> String,
}

impl Meter {
    /// `told` is what the viewer was told when the session began.
    pub fn new(usage: Usage, told: Allowance, now: Instant) -> Self {
        Meter {
            usage,
            told,
            warned: false,
            since: now,
            save: true,
            today: crate::dates::today,
        }
    }

    /// Counts the time since the last reading, once a minute.
    pub fn read(&mut self, now: Instant) -> Reading {
        let elapsed = now.saturating_duration_since(self.since);
        if elapsed < Duration::from_secs(60) {
            return Reading::Nothing;
        }
        self.since = now;
        let today = &(self.today)();
        if self.save {
            // A host and a viewer on this computer may both be counting.
            self.usage = load();
        }
        self.usage.add(today, elapsed.as_secs());
        if self.save
            && let Err(e) = save(&self.usage)
        {
            tracing::warn!("could not save the company hours: {e:#}");
        }
        let now = self.usage.allowance(today);
        let reading = match now {
            Allowance::Used => Reading::Over,
            Allowance::Marked { minutes_left } | Allowance::Clear { minutes_left }
                if !self.warned && u64::from(minutes_left) * 60 <= WARNING.as_secs() =>
            {
                self.warned = true;
                Reading::Tell(now)
            }
            _ if std::mem::discriminant(&now) != std::mem::discriminant(&self.told) => {
                Reading::Tell(now)
            }
            _ => Reading::Nothing,
        };
        if let Reading::Tell(told) = reading {
            self.told = told;
        }
        reading
    }

    /// Counts the rest of the session, short of a minute, when it ends:
    /// otherwise reconnecting every 59 seconds would never count.
    pub fn finish(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.since).as_secs();
        self.since = now;
        if elapsed == 0 {
            return;
        }
        if self.save {
            self.usage = load();
        }
        self.usage.add(&(self.today)(), elapsed);
        if self.save
            && let Err(e) = save(&self.usage)
        {
            tracing::warn!("could not save the company hours: {e:#}");
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.finish(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after_trial() -> Usage {
        Usage {
            first_seen: Some("2026-09-01".into()),
            ..Usage::default()
        }
    }

    #[test]
    fn a_company_computer_has_a_trial_then_monthly_hours() {
        let mut usage = Usage::default();
        usage.seen("2026-10-02");
        assert_eq!(
            usage.allowance("2026-10-02"),
            Allowance::Trial { days_left: 14 }
        );
        assert_eq!(
            usage.allowance("2026-10-15"),
            Allowance::Trial { days_left: 1 }
        );
        usage.add("2026-10-10", 10 * 3600);
        assert_eq!(usage.seconds, 0, "the trial is not counted");
        assert_eq!(
            usage.allowance("2026-10-16"),
            Allowance::Clear { minutes_left: 480 }
        );

        let mut usage = after_trial();
        usage.add("2026-10-02", 2 * 3600 - 1);
        assert!(matches!(
            usage.allowance("2026-10-02"),
            Allowance::Clear { .. }
        ));
        usage.add("2026-10-02", 1);
        assert_eq!(
            usage.allowance("2026-10-02"),
            Allowance::Marked { minutes_left: 360 }
        );
        usage.add("2026-10-20", 6 * 3600);
        assert_eq!(usage.allowance("2026-10-20"), Allowance::Used);
        assert_eq!(
            usage.allowance("2026-11-01"),
            Allowance::Clear { minutes_left: 480 },
            "a new month starts again"
        );
        usage.add("2026-11-01", 60);
        assert_eq!(usage.month, "2026-11");
        assert_eq!(usage.seconds, 60);
    }

    #[test]
    fn a_session_is_told_when_the_mark_starts_warned_and_ended() {
        let mut usage = after_trial();
        usage.add("2026-10-02", 2 * 3600 - 120);
        let start = Instant::now();
        let mut meter = Meter::new(usage.clone(), usage.allowance("2026-10-02"), start);
        meter.save = false;
        meter.today = || "2026-10-02".into();
        let at = |minutes: u64| start + Duration::from_secs(minutes * 60);

        assert_eq!(
            meter.read(start + Duration::from_secs(30)),
            Reading::Nothing
        );
        assert_eq!(meter.read(at(1)), Reading::Nothing);
        assert_eq!(
            meter.read(at(2)),
            Reading::Tell(Allowance::Marked { minutes_left: 360 })
        );
        // 5 h 50 min later: 10 minutes left.
        assert_eq!(
            meter.read(at(2 + 350)),
            Reading::Tell(Allowance::Marked { minutes_left: 10 })
        );
        assert_eq!(meter.read(at(2 + 355)), Reading::Nothing);
        assert_eq!(meter.read(at(2 + 360)), Reading::Over);
    }

    #[test]
    fn a_short_session_still_counts() {
        let usage = after_trial();
        let start = Instant::now();
        let mut meter = Meter::new(usage.clone(), usage.allowance("2026-10-02"), start);
        meter.save = false;
        meter.today = || "2026-10-02".into();
        assert_eq!(
            meter.read(start + Duration::from_secs(59)),
            Reading::Nothing
        );
        meter.finish(start + Duration::from_secs(59));
        assert_eq!(meter.usage.seconds, 59);
        meter.finish(start + Duration::from_secs(59));
        assert_eq!(meter.usage.seconds, 59, "counted once");
    }

    #[test]
    fn names_and_times_read_well() {
        let corp = Management {
            domain: Some("CORP".into()),
            ..Management::default()
        };
        assert!(corp.managed());
        assert_eq!(corp.name(), "CORP");
        let intune = Management {
            device_management: true,
            ..Management::default()
        };
        assert_eq!(intune.name(), "an organisation");
        assert!(!Management::default().managed());
        assert_eq!(hours_and_minutes(320), "5 h 20 min");
        assert_eq!(hours_and_minutes(40), "40 min");
        assert_eq!(hours_and_minutes(120), "2 h");
        assert_eq!(
            Allowance::Trial { days_left: 9 }.describe("CORP"),
            "Company computer (CORP) without a TideDesk licence: trial, 9 days left."
        );
        // A home lab's own domain may be declared; a company's cannot.
        assert!(corp.may_declare());
        let declared = Declaration {
            user: "ana".into(),
            date: "2026-10-02".into(),
            domain: "CORP".into(),
        };
        assert!(declared.holds(&corp));
        let other = Management {
            domain: Some("HOME".into()),
            ..Management::default()
        };
        assert!(!declared.holds(&other), "another domain");
        let enrolled = Management {
            device_management: true,
            ..corp.clone()
        };
        assert!(!enrolled.may_declare() && !declared.holds(&enrolled));
        let entra = Management {
            entra: Some("Contoso".into()),
            ..corp.clone()
        };
        assert!(!entra.may_declare() && !declared.holds(&entra));
        assert!(!intune.may_declare());
        assert_eq!(
            declared.describe(),
            "Declared personal by ana on 2026-10-02 (domain CORP)."
        );
        // Reading Windows' records works on any computer.
        let _ = management();
    }
}
