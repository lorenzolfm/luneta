use std::fmt;
use std::num::NonZeroU64;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::said::Said;
use crate::v1;

pub const TELEMETRY_TEXT_BYTES_MAX: usize = 128;

pub const USAGE_WINDOWS_MAX: usize = 4;

pub const USED_PERMILLE_MAX: u16 = 1000;

const _: () = assert!(TELEMETRY_TEXT_BYTES_MAX >= 32);
const _: () = assert!(USAGE_WINDOWS_MAX > 0);
const _: () = assert!(USED_PERMILLE_MAX == 1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsedPermille(u16);

impl UsedPermille {
    pub const ZERO: Self = Self(0);
    pub const FULL: Self = Self(USED_PERMILLE_MAX);

    #[must_use]
    pub const fn try_new(value: u16) -> Option<Self> {
        if value <= USED_PERMILLE_MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub fn of_percent(percent: f64) -> Option<Self> {
        if !percent.is_finite() {
            return None;
        }
        let clamped = percent.clamp(0.0, 100.0);
        let scaled = (clamped * 10.0).round();
        let value = u16::try_from(scaled as u64).ok()?.min(USED_PERMILLE_MAX);
        assert!(value <= USED_PERMILLE_MAX);
        Some(Self(value))
    }

    #[must_use]
    pub fn of_remaining_fraction(remaining: f64) -> Option<Self> {
        if !remaining.is_finite() {
            return None;
        }
        let used = 1.0 - remaining.clamp(0.0, 1.0);
        Self::of_percent(used * 100.0)
    }

    #[must_use]
    pub fn of_ratio(used: u64, total: NonZeroU64) -> Self {
        let total = u128::from(total.get());
        let scaled = u128::from(used)
            .saturating_mul(u128::from(USED_PERMILLE_MAX))
            .saturating_add(total / 2)
            / total;
        let value = u16::try_from(scaled)
            .unwrap_or(USED_PERMILLE_MAX)
            .min(USED_PERMILLE_MAX);
        assert!(value <= USED_PERMILLE_MAX);
        Self(value)
    }

    #[must_use]
    pub fn of_api(value: u32) -> Option<Self> {
        u16::try_from(value).ok().and_then(Self::try_new)
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    #[must_use]
    pub const fn whole_percent_rounded(self) -> u8 {
        ((self.0 + 5) / 10) as u8
    }

    #[must_use]
    pub const fn whole_percent_floor(self) -> u8 {
        (self.0 / 10) as u8
    }

    #[must_use]
    pub const fn tenths(self) -> u8 {
        (self.0 % 10) as u8
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TelemetryText(Said);

impl TelemetryText {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let mut cleaned = String::new();
        let mut bytes = 0usize;
        let mut pending_space = false;
        for ch in raw.chars() {
            if is_stripped(ch) {
                continue;
            }
            if ch.is_whitespace() {
                if !cleaned.is_empty() {
                    pending_space = true;
                }
                continue;
            }
            let width = ch.len_utf8();
            let space_bytes = usize::from(pending_space);
            if bytes + space_bytes + width > TELEMETRY_TEXT_BYTES_MAX {
                break;
            }
            if pending_space {
                cleaned.push(' ');
                bytes += 1;
                pending_space = false;
            }
            cleaned.push(ch);
            bytes += width;
        }
        assert!(cleaned.len() <= TELEMETRY_TEXT_BYTES_MAX);
        let said = Said::new(cleaned)?;
        Some(Self(said))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for TelemetryText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn is_stripped(ch: char) -> bool {
    let code = u32::from(ch);
    ch.is_control()
        || matches!(
            code,
            0x00ad
                | 0x0600..=0x0605
                | 0x061c
                | 0x06dd
                | 0x070f
                | 0x0890..=0x0891
                | 0x08e2
                | 0x180e
                | 0x200b..=0x200f
                | 0x202a..=0x202e
                | 0x2060..=0x2064
                | 0x2066..=0x206f
                | 0xfeff
                | 0xfff9..=0xfffb
                | 0x110bd
                | 0x110cd
                | 0x13430..=0x1343f
                | 0x1bca0..=0x1bca3
                | 0x1d173..=0x1d17a
                | 0xe0001
                | 0xe0020..=0xe007f
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextWindow {
    pub used_permille: UsedPermille,
    pub window_tokens: Option<NonZeroU64>,
    pub used_tokens: Option<u64>,
}

impl ContextWindow {
    #[must_use]
    pub fn to_api(self) -> v1::ContextWindow {
        v1::ContextWindow {
            used_permille: u32::from(self.used_permille.get()),
            window_tokens: self.window_tokens.map(NonZeroU64::get),
            used_tokens: self.used_tokens,
        }
    }

    #[must_use]
    pub fn of_api(wire: &v1::ContextWindow) -> Option<Self> {
        let used_permille = UsedPermille::of_api(wire.used_permille)?;
        let window_tokens = wire.window_tokens.and_then(NonZeroU64::new);
        Some(Self {
            used_permille,
            window_tokens,
            used_tokens: wire.used_tokens,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageWindow {
    pub label: TelemetryText,
    pub used_permille: UsedPermille,
    pub resets_at: Option<SystemTime>,
}

impl UsageWindow {
    #[must_use]
    pub fn to_api(&self) -> v1::UsageWindow {
        v1::UsageWindow {
            label: self.label.as_str().to_owned(),
            used_permille: u32::from(self.used_permille.get()),
            resets_at: self.resets_at.map(Into::into),
        }
    }

    #[must_use]
    pub fn of_api(wire: &v1::UsageWindow) -> Option<Self> {
        let label = TelemetryText::parse(&wire.label)?;
        let used_permille = UsedPermille::of_api(wire.used_permille)?;
        let resets_at = wire.resets_at.and_then(|at| SystemTime::try_from(at).ok());
        Some(Self {
            label,
            used_permille,
            resets_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentUsage {
    windows: Vec<UsageWindow>,
    session_cost_micro_usd: Option<u64>,
}

impl AgentUsage {
    #[must_use]
    pub fn new(mut windows: Vec<UsageWindow>, session_cost_micro_usd: Option<u64>) -> Option<Self> {
        windows.truncate(USAGE_WINDOWS_MAX);
        assert!(windows.len() <= USAGE_WINDOWS_MAX);
        if windows.is_empty() && session_cost_micro_usd.is_none() {
            return None;
        }
        Some(Self {
            windows,
            session_cost_micro_usd,
        })
    }

    #[must_use]
    pub fn windows(&self) -> &[UsageWindow] {
        &self.windows
    }

    #[must_use]
    pub const fn session_cost_micro_usd(&self) -> Option<u64> {
        self.session_cost_micro_usd
    }

    #[must_use]
    pub fn has_windows(&self) -> bool {
        !self.windows.is_empty()
    }

    #[must_use]
    pub fn to_api(&self) -> v1::AgentUsage {
        assert!(self.windows.len() <= USAGE_WINDOWS_MAX);
        v1::AgentUsage {
            windows: self.windows.iter().map(UsageWindow::to_api).collect(),
            session_cost_micro_usd: self.session_cost_micro_usd,
        }
    }

    #[must_use]
    pub fn of_api(wire: &v1::AgentUsage) -> Option<Self> {
        let windows: Vec<UsageWindow> = wire
            .windows
            .iter()
            .take(USAGE_WINDOWS_MAX)
            .filter_map(UsageWindow::of_api)
            .collect();
        Self::new(windows, wire.session_cost_micro_usd)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTelemetry {
    model: Option<TelemetryText>,
    effort: Option<TelemetryText>,
    context: Option<ContextWindow>,
    usage: Option<AgentUsage>,
}

impl AgentTelemetry {
    #[must_use]
    pub fn new(
        model: Option<TelemetryText>,
        effort: Option<TelemetryText>,
        context: Option<ContextWindow>,
        usage: Option<AgentUsage>,
    ) -> Option<Self> {
        if model.is_none() && effort.is_none() && context.is_none() && usage.is_none() {
            return None;
        }
        Some(Self {
            model,
            effort,
            context,
            usage,
        })
    }

    #[must_use]
    pub fn model(&self) -> Option<&TelemetryText> {
        self.model.as_ref()
    }

    #[must_use]
    pub fn effort(&self) -> Option<&TelemetryText> {
        self.effort.as_ref()
    }

    #[must_use]
    pub const fn context(&self) -> Option<ContextWindow> {
        self.context
    }

    #[must_use]
    pub fn usage(&self) -> Option<&AgentUsage> {
        self.usage.as_ref()
    }

    #[must_use]
    pub fn to_api(&self) -> v1::AgentTelemetry {
        v1::AgentTelemetry {
            model: self.model.as_ref().map(|text| text.as_str().to_owned()),
            effort: self.effort.as_ref().map(|text| text.as_str().to_owned()),
            context: self.context.map(ContextWindow::to_api),
            usage: self.usage.as_ref().map(AgentUsage::to_api),
        }
    }

    #[must_use]
    pub fn of_api(wire: &v1::AgentTelemetry) -> Option<Self> {
        let model = wire.model.as_deref().and_then(TelemetryText::parse);
        let effort = wire.effort.as_deref().and_then(TelemetryText::parse);
        let context = wire.context.as_ref().and_then(ContextWindow::of_api);
        let usage = wire.usage.as_ref().and_then(AgentUsage::of_api);
        Self::new(model, effort, context, usage)
    }
}

#[must_use]
pub fn cost_usd_to_micro_usd(usd: f64) -> Option<u64> {
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    let micro = (usd * 1_000_000.0).round();
    if micro > (u64::MAX as f64) {
        return None;
    }
    Some(micro as u64)
}

#[must_use]
pub fn strip_effort_suffix<'a>(model: &'a str, effort: &str) -> &'a str {
    let trimmed = model.trim();
    let effort = effort.trim();
    if effort.is_empty() {
        return trimmed;
    }
    let Some(without_close) = trimmed.strip_suffix(')') else {
        return trimmed;
    };
    let Some((prefix, inside)) = without_close.rsplit_once('(') else {
        return trimmed;
    };
    let prefix = prefix.trim_end();
    if !prefix.is_empty() && inside.trim().eq_ignore_ascii_case(effort) {
        prefix
    } else {
        trimmed
    }
}

#[must_use]
pub fn parse_rfc3339_utc(raw: &str) -> Option<SystemTime> {
    let raw = raw.trim();
    let without_z = raw.strip_suffix('Z').or_else(|| raw.strip_suffix('z'))?;
    let (date, time_full) = without_z
        .split_once('T')
        .or_else(|| without_z.split_once('t'))?;
    let (time, _frac) = time_full.split_once('.').unwrap_or((time_full, ""));
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let secs = u64::try_from(days)
        .ok()?
        .checked_mul(86_400)?
        .checked_add(u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second))?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = u64::try_from(y - era * 400).ok()?;
    let m = u64::from(month);
    let d = u64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + i64::try_from(doe).ok()? - 719_468;
    (days >= 0).then_some(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_permille_bounds_percentages_ratios_and_wire_values() {
        assert_eq!(UsedPermille::try_new(0), Some(UsedPermille::ZERO));
        assert_eq!(UsedPermille::try_new(1000), Some(UsedPermille::FULL));
        assert_eq!(UsedPermille::try_new(1001), None);

        assert_eq!(UsedPermille::of_percent(1.94).unwrap().get(), 19);
        assert_eq!(UsedPermille::of_percent(14.26).unwrap().get(), 143);
        assert_eq!(UsedPermille::of_percent(-5.0), Some(UsedPermille::ZERO));
        assert_eq!(UsedPermille::of_percent(140.0), Some(UsedPermille::FULL));
        assert_eq!(UsedPermille::of_percent(f64::NAN), None);
        assert_eq!(UsedPermille::of_percent(f64::INFINITY), None);

        assert_eq!(
            UsedPermille::of_remaining_fraction(0.9378).unwrap().get(),
            62
        );
        assert_eq!(
            UsedPermille::of_remaining_fraction(1.0),
            Some(UsedPermille::ZERO)
        );
        assert_eq!(
            UsedPermille::of_remaining_fraction(0.0),
            Some(UsedPermille::FULL)
        );

        let million = NonZeroU64::new(1_000_000).unwrap();
        assert_eq!(UsedPermille::of_ratio(19_000, million).get(), 19);
        assert_eq!(
            UsedPermille::of_ratio(2_000_000, million),
            UsedPermille::FULL
        );

        assert_eq!(UsedPermille::of_api(1000), Some(UsedPermille::FULL));
        assert_eq!(UsedPermille::of_api(1001), None);
        assert_eq!(
            UsedPermille::try_new(246).unwrap().whole_percent_rounded(),
            25
        );
        assert_eq!(
            UsedPermille::try_new(244).unwrap().whole_percent_rounded(),
            24
        );
        assert_eq!(UsedPermille::try_new(19).unwrap().whole_percent_floor(), 1);
        assert_eq!(UsedPermille::try_new(19).unwrap().tenths(), 9);
    }

    #[test]
    fn telemetry_text_sanitizes_control_and_invisible_characters_and_bounds_length() {
        assert_eq!(TelemetryText::parse("   \n\t  "), None);
        assert_eq!(
            TelemetryText::parse("  Opus\u{200b}  4.7\n\r\x1b[31m  ")
                .unwrap()
                .as_str(),
            "Opus 4.7[31m"
        );
        let long = "a".repeat(TELEMETRY_TEXT_BYTES_MAX + 50);
        assert_eq!(
            TelemetryText::parse(&long).unwrap().as_str().len(),
            TELEMETRY_TEXT_BYTES_MAX
        );
    }

    #[test]
    fn telemetry_round_trips_and_rejects_empty_and_overlong_wire_shapes() {
        assert_eq!(AgentUsage::new(Vec::new(), None), None);
        assert_eq!(AgentTelemetry::new(None, None, None, None), None);

        let reset = UNIX_EPOCH + Duration::from_secs(1_770_000_007);
        let windows = (0..=USAGE_WINDOWS_MAX)
            .map(|index| UsageWindow {
                label: TelemetryText::parse(&format!("w{index}")).unwrap(),
                used_permille: UsedPermille::try_new(250).unwrap(),
                resets_at: Some(reset),
            })
            .collect();
        let usage = AgentUsage::new(windows, Some(260_000)).unwrap();
        assert_eq!(usage.windows().len(), USAGE_WINDOWS_MAX);

        let telemetry = AgentTelemetry::new(
            TelemetryText::parse("Opus 4.7"),
            TelemetryText::parse("high"),
            Some(ContextWindow {
                used_permille: UsedPermille::try_new(19).unwrap(),
                window_tokens: NonZeroU64::new(1_000_000),
                used_tokens: Some(19_000),
            }),
            Some(usage),
        )
        .unwrap();
        let wire = telemetry.to_api();
        assert_eq!(AgentTelemetry::of_api(&wire), Some(telemetry));

        let empty_wire = v1::AgentTelemetry::default();
        assert_eq!(AgentTelemetry::of_api(&empty_wire), None);

        let invalid_context = v1::AgentTelemetry {
            context: Some(v1::ContextWindow {
                used_permille: 1500,
                window_tokens: Some(0),
                used_tokens: None,
            }),
            ..v1::AgentTelemetry::default()
        };
        assert_eq!(AgentTelemetry::of_api(&invalid_context), None);
    }

    #[test]
    fn strip_effort_suffix_removes_matching_parenthetical_only() {
        assert_eq!(
            strip_effort_suffix("Gemini 4 Neon (XHigh)", "xhigh"),
            "Gemini 4 Neon"
        );
        assert_eq!(
            strip_effort_suffix("Gemini 4 Argon (Medium)", "medium"),
            "Gemini 4 Argon"
        );
        assert_eq!(
            strip_effort_suffix("Opus 4.7 (1M context)", "high"),
            "Opus 4.7 (1M context)"
        );
        assert_eq!(strip_effort_suffix("(high)", "high"), "(high)");
    }

    #[test]
    fn parse_rfc3339_utc_handles_known_dates_and_rejects_invalid_ones() {
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(UNIX_EPOCH));
        assert_eq!(
            parse_rfc3339_utc("2026-07-06T07:50:32Z"),
            Some(UNIX_EPOCH + Duration::from_secs(1_783_324_232))
        );
        assert_eq!(
            parse_rfc3339_utc("2024-02-29T12:30:45.123Z"),
            Some(UNIX_EPOCH + Duration::from_secs(1_709_209_845))
        );
        for bad in [
            "",
            "2026-02-29T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-07-06T24:00:00Z",
            "2026-07-06T07:50:32+02:00",
            "not-a-date",
        ] {
            assert_eq!(parse_rfc3339_utc(bad), None, "{bad:?}");
        }
    }
}
