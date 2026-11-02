//! Zone offsets from the system zoneinfo database, read the way CPython's `zoneinfo` reads it.
//!
//! `bin/fm-audit-envelope.py` resolves a record's `user.timezone` with `zoneinfo.ZoneInfo`. That
//! searches the zoneinfo directories in order and reads the first TZif file it finds. This reader
//! follows the same steps: the transition table, the first standard-time type before the first
//! transition, and the POSIX footer rule for instants after the last transition. Offsets keep
//! their seconds; nothing is rounded to minutes.
//!
//! There is no zoneinfo directory on Windows here, so every zone name is unknown there and a
//! record that names a zone is refused. That is a documented gap, not a silent pass.

use std::path::Path;

/// The directories `zoneinfo` searches for TZif files, in its order.
const ZONEINFO_DIRS: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/usr/lib/zoneinfo",
    "/usr/share/lib/zoneinfo",
    "/etc/zoneinfo",
];

/// Returns the UTC offset in seconds of `zone` at `utc_epoch_secs`, or `None` when the zone name
/// is not a relative key, no TZif file holds it, or the file cannot be read.
pub fn utc_offset_secs(zone: &str, utc_epoch_secs: i64) -> Option<i32> {
    let bytes = read_zone_file(zone)?;
    Some(Table::parse(&bytes)?.offset_at(utc_epoch_secs))
}

/// Reads the first zone file that exists for `zone`, after the key checks `zoneinfo` applies.
fn read_zone_file(zone: &str) -> Option<Vec<u8>> {
    let relative = !zone.is_empty()
        && !zone.starts_with('/')
        && !zone.contains('\0')
        && zone
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if !relative {
        return None;
    }
    ZONEINFO_DIRS
        .iter()
        .find_map(|dir| std::fs::read(Path::new(dir).join(zone)).ok())
}

/// The parsed transition table of one TZif file, plus its POSIX footer rule when present.
#[derive(Debug, PartialEq)]
struct Table {
    transitions: Vec<i64>,
    transition_types: Vec<usize>,
    types: Vec<LocalType>,
    footer: Option<Footer>,
}

/// One local time type: its UTC offset in seconds and whether it is daylight time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LocalType {
    gmtoff: i32,
    isdst: bool,
}

/// The counts in one TZif header, in the order RFC 8536 lists them.
#[derive(Debug, Clone, Copy)]
struct Counts {
    isut: usize,
    isstd: usize,
    leap: usize,
    time: usize,
    types: usize,
    chars: usize,
}

impl Counts {
    /// Reads the header at `at`, or `None` when the magic is absent or the header is truncated.
    fn read(bytes: &[u8], at: usize) -> Option<Self> {
        if bytes.get(at..at + 4)? != b"TZif" {
            return None;
        }
        let field = |index: usize| -> Option<usize> {
            let start = at + 20 + 4 * index;
            let raw: [u8; 4] = bytes.get(start..start + 4)?.try_into().ok()?;
            usize::try_from(u32::from_be_bytes(raw)).ok()
        };
        Some(Self {
            isut: field(0)?,
            isstd: field(1)?,
            leap: field(2)?,
            time: field(3)?,
            types: field(4)?,
            chars: field(5)?,
        })
    }

    /// The byte length of the data block that follows a header, given the width of a time field.
    fn block_len(self, time_size: usize) -> usize {
        self.time * time_size
            + self.time
            + self.types * 6
            + self.chars
            + self.leap * (time_size + 4)
            + self.isstd
            + self.isut
    }
}

impl Table {
    /// Parses one TZif file. Version 1 files carry 32-bit times only; later versions carry a
    /// 64-bit block after the 32-bit one and end with the footer rule.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let version = *bytes.get(4)?;
        let v1 = Counts::read(bytes, 0)?;
        if version == 0 {
            return Self::read_block(bytes, 44, v1, 4, None);
        }
        let second = 44 + v1.block_len(4);
        let v2 = Counts::read(bytes, second)?;
        let data = second + 44;
        let end = data + v2.block_len(8);
        let footer = match footer_text(bytes.get(end..)?) {
            Some(text) => Some(Footer::parse(text)?),
            None => None,
        };
        Self::read_block(bytes, data, v2, 8, footer)
    }

    /// Reads one data block whose transition times are `time_size` bytes wide.
    fn read_block(
        bytes: &[u8],
        start: usize,
        counts: Counts,
        time_size: usize,
        footer: Option<Footer>,
    ) -> Option<Self> {
        let mut at = start;
        let times = bytes.get(at..at + counts.time * time_size)?;
        at += counts.time * time_size;
        let indices = bytes.get(at..at + counts.time)?;
        at += counts.time;
        let rows = bytes.get(at..at + counts.types * 6)?;
        if counts.types == 0 {
            return None;
        }
        let transitions = times
            .chunks_exact(time_size)
            .map(read_time)
            .collect::<Option<Vec<i64>>>()?;
        let transition_types = indices
            .iter()
            .map(|&index| {
                let index = usize::from(index);
                (index < counts.types).then_some(index)
            })
            .collect::<Option<Vec<usize>>>()?;
        let types = rows
            .chunks_exact(6)
            .map(|row| LocalType {
                gmtoff: i32::from_be_bytes([row[0], row[1], row[2], row[3]]),
                isdst: row[4] != 0,
            })
            .collect();
        Some(Self {
            transitions,
            transition_types,
            types,
            footer,
        })
    }

    /// The type in force before the first transition: the first standard-time type in file
    /// order, else the first type.
    fn before_first(&self) -> LocalType {
        self.types
            .iter()
            .copied()
            .find(|local| !local.isdst)
            .unwrap_or(self.types[0])
    }

    /// The UTC offset in seconds at `utc_epoch_secs`. After the last transition the footer rule
    /// decides, as in `zoneinfo`.
    fn offset_at(&self, utc_epoch_secs: i64) -> i32 {
        let passed = self.transitions.partition_point(|&at| at <= utc_epoch_secs);
        if passed == self.transitions.len()
            && let Some(footer) = &self.footer
        {
            return footer.offset_at(utc_epoch_secs);
        }
        match passed {
            0 => self.before_first().gmtoff,
            index => self.types[self.transition_types[index - 1]].gmtoff,
        }
    }
}

/// Reads one transition time of 4 or 8 bytes, big-endian and signed.
fn read_time(chunk: &[u8]) -> Option<i64> {
    match chunk {
        [a, b, c, d] => Some(i64::from(i32::from_be_bytes([*a, *b, *c, *d]))),
        _ => <[u8; 8]>::try_from(chunk).ok().map(i64::from_be_bytes),
    }
}

/// The footer text between the two newlines that end a v2+ file, or `None` when it is empty.
fn footer_text(tail: &[u8]) -> Option<&str> {
    let body = tail.strip_prefix(b"\n")?;
    let end = body.iter().position(|&byte| byte == b'\n')?;
    let text = std::str::from_utf8(&body[..end]).ok()?;
    (!text.is_empty()).then_some(text)
}

/// A POSIX TZ rule: a fixed offset, or a standard and daylight pair with yearly transitions.
#[derive(Debug, Clone, PartialEq)]
enum Footer {
    Fixed(i32),
    Daylight {
        std: i32,
        dst: i32,
        start: Rule,
        end: Rule,
    },
}

/// One yearly transition: the date rule and the local wall-clock time it happens at.
#[derive(Debug, Clone, PartialEq)]
struct Rule {
    date: DateRule,
    local_secs: i64,
}

/// A POSIX date: `Jn` (1 to 365, Feb 29 never counted), `n` (0 to 365), or `Mm.w.d`.
#[derive(Debug, Clone, PartialEq)]
enum DateRule {
    Julian(i64),
    ZeroBased(i64),
    MonthWeekDay { month: i64, week: i64, day: i64 },
}

impl Footer {
    /// Parses a footer such as `CST6CDT,M3.2.0,M11.1.0`. Returns `None` for any form outside
    /// the POSIX subset used by zoneinfo files.
    fn parse(text: &str) -> Option<Self> {
        let mut cursor = Cursor::new(text);
        cursor.name()?;
        let std = -cursor.offset()?;
        if cursor.at_end() {
            return Some(Self::Fixed(std));
        }
        cursor.name()?;
        let dst = if cursor.peek() == Some(',') {
            std + 3600
        } else {
            -cursor.offset()?
        };
        cursor.expect(',')?;
        let start = cursor.rule()?;
        cursor.expect(',')?;
        let end = cursor.rule()?;
        cursor.at_end().then_some(Self::Daylight {
            std,
            dst,
            start,
            end,
        })
    }

    /// The UTC offset in seconds at `utc_epoch_secs`, under this rule.
    fn offset_at(&self, utc_epoch_secs: i64) -> i32 {
        match self {
            Self::Fixed(offset) => *offset,
            Self::Daylight {
                std,
                dst,
                start,
                end,
            } => {
                let year = civil_from_days((utc_epoch_secs + i64::from(*std)).div_euclid(86_400)).0;
                let start_utc = start.instant(year) - i64::from(*std);
                let end_utc = end.instant(year) - i64::from(*dst);
                let in_dst = if start_utc < end_utc {
                    start_utc <= utc_epoch_secs && utc_epoch_secs < end_utc
                } else {
                    !(end_utc <= utc_epoch_secs && utc_epoch_secs < start_utc)
                };
                if in_dst { *dst } else { *std }
            }
        }
    }
}

impl Rule {
    /// The local wall-clock instant of this transition in `year`, as seconds since the epoch.
    fn instant(&self, year: i64) -> i64 {
        self.date.day_number(year) * 86_400 + self.local_secs
    }
}

impl DateRule {
    /// The day number since 1970-01-01 of this rule in `year`.
    fn day_number(&self, year: i64) -> i64 {
        let jan1 = days_from_civil(year, 1, 1);
        match *self {
            Self::Julian(n) => jan1 + n - 1 + i64::from(is_leap(year) && n >= 60),
            Self::ZeroBased(n) => jan1 + n,
            Self::MonthWeekDay { month, week, day } => {
                let first = days_from_civil(year, month, 1);
                let weekday_of_first = (first + 4).rem_euclid(7);
                let mut date = first + (day - weekday_of_first).rem_euclid(7) + (week - 1) * 7;
                while civil_from_days(date).1 != month {
                    date -= 7;
                }
                date
            }
        }
    }
}

/// A forward-only reader over one footer string.
struct Cursor<'a> {
    rest: &'a str,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { rest: text }
    }

    fn at_end(&self) -> bool {
        self.rest.is_empty()
    }

    fn peek(&self) -> Option<char> {
        self.rest.chars().next()
    }

    fn expect(&mut self, wanted: char) -> Option<()> {
        self.rest = self.rest.strip_prefix(wanted)?;
        Some(())
    }

    /// Reads a zone abbreviation: a `<...>` quoted name, or at least three letters.
    fn name(&mut self) -> Option<usize> {
        if let Some(quoted) = self.rest.strip_prefix('<') {
            let end = quoted.find('>')?;
            self.rest = &quoted[end + 1..];
            return Some(end);
        }
        let len = self
            .rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(self.rest.len());
        if len < 3 {
            return None;
        }
        self.rest = &self.rest[len..];
        Some(len)
    }

    /// Reads a signed `hh[:mm[:ss]]` value in seconds, with the sign as written.
    fn offset(&mut self) -> Option<i32> {
        let sign = match self.peek() {
            Some('-') => {
                self.rest = &self.rest[1..];
                -1
            }
            Some('+') => {
                self.rest = &self.rest[1..];
                1
            }
            _ => 1,
        };
        Some(sign * self.clock()?)
    }

    /// Reads `hh[:mm[:ss]]` with hours up to 167, as RFC 8536 allows in a footer.
    fn clock(&mut self) -> Option<i32> {
        let hours = self.number()?;
        if hours > 167 {
            return None;
        }
        let mut total = i32::try_from(hours * 3600).ok()?;
        for scale in [60, 1] {
            if !self.rest.starts_with(':') {
                break;
            }
            self.rest = &self.rest[1..];
            let part = self.number()?;
            if part > 59 {
                return None;
            }
            total += i32::try_from(part * scale).ok()?;
        }
        Some(total)
    }

    /// Reads one transition rule: a date and an optional `/time`, which defaults to 02:00:00.
    fn rule(&mut self) -> Option<Rule> {
        let date = self.date()?;
        let local_secs = match self.peek() {
            Some('/') => {
                self.rest = &self.rest[1..];
                i64::from(self.offset()?)
            }
            _ => 7200,
        };
        Some(Rule { date, local_secs })
    }

    fn date(&mut self) -> Option<DateRule> {
        if let Some(rest) = self.rest.strip_prefix('M') {
            self.rest = rest;
            let month = self.number()?;
            self.expect('.')?;
            let week = self.number()?;
            self.expect('.')?;
            let day = self.number()?;
            if !(1..=12).contains(&month) || !(1..=5).contains(&week) || !(0..=6).contains(&day) {
                return None;
            }
            return Some(DateRule::MonthWeekDay { month, week, day });
        }
        if let Some(rest) = self.rest.strip_prefix('J') {
            self.rest = rest;
            let n = self.number()?;
            return (1..=365).contains(&n).then_some(DateRule::Julian(n));
        }
        let n = self.number()?;
        (0..=365).contains(&n).then_some(DateRule::ZeroBased(n))
    }

    fn number(&mut self) -> Option<i64> {
        let len = self
            .rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(self.rest.len());
        if len == 0 {
            return None;
        }
        let value = self.rest[..len].parse().ok()?;
        self.rest = &self.rest[len..];
        Some(value)
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The `(year, month, day)` of a day number since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_round_trip_across_leap_years() {
        for days in [-719_162, -1, 0, 10_957, 11_016, 20_454, 2_932_896] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn footer_rule_gives_the_daylight_offset_inside_the_window_only() {
        let chicago = Footer::parse("CST6CDT,M3.2.0,M11.1.0").expect("a valid footer");
        // 2040-07-01T12:00:00Z is inside daylight time (CDT, UTC-5).
        assert_eq!(chicago.offset_at(2_222_251_200), -18_000);
        // 2040-01-15T12:00:00Z is standard time (CST, UTC-6).
        assert_eq!(chicago.offset_at(2_212_012_800), -21_600);
    }

    #[test]
    fn footer_forms_outside_the_supported_subset_are_refused() {
        assert_eq!(Footer::parse("UTC0"), Some(Footer::Fixed(0)));
        assert!(Footer::parse("<+03>-3").is_some());
        assert!(Footer::parse("CST6CDT,M13.2.0,M11.1.0").is_none());
        assert!(Footer::parse("CST6CDT,M3.2.0").is_none());
        assert!(Footer::parse("X6").is_none());
    }

    /// Each row is `zone, UTC epoch seconds, offset seconds` as CPython's `zoneinfo` reports it on
    /// the host that recorded the table. The rows cover LMT, DST edges, southern hemisphere, a
    /// zone without DST, and instants after the last transition that only the footer covers.
    #[cfg(unix)]
    #[test]
    fn offsets_match_the_recorded_cpython_zoneinfo_table() {
        let table = include_str!("../../tests/fixtures/audit-validate/tzif-offsets.tsv");
        let mut checked = 0;
        for line in table.lines().filter(|line| !line.starts_with('#')) {
            let mut fields = line.split('\t');
            let zone = fields.next().expect("a zone name");
            let epoch: i64 = fields
                .next()
                .expect("an epoch")
                .parse()
                .expect("an integer epoch");
            let offset: i32 = fields
                .next()
                .expect("an offset")
                .parse()
                .expect("an integer offset");
            assert_eq!(
                utc_offset_secs(zone, epoch),
                Some(offset),
                "{zone} at {epoch}"
            );
            checked += 1;
        }
        assert_eq!(checked, 153);
    }

    #[test]
    fn unknown_or_escaping_zone_names_are_not_read() {
        assert_eq!(utc_offset_secs("", 0), None);
        assert_eq!(utc_offset_secs("/etc/localtime", 0), None);
        assert_eq!(utc_offset_secs("../etc/passwd", 0), None);
        assert_eq!(utc_offset_secs("America/../UTC", 0), None);
        assert_eq!(utc_offset_secs("no/such/zone", 0), None);
    }
}
