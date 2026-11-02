//! `mcao herdr lab-viewer <session> <pidfile>` (DPES-864): attaches one foreground Herdr viewer to
//! a named lab session over a pty. It replaces `bin/fm-herdr-lab-viewer.py`, which was removed.
//! It matches that script in its arguments, child command, pty size, SIGTERM relay, and exit
//! codes (127 when the exec fails, 3 for pty, fork, or pidfile setup, 2 for a refused argument).
//! Its messages keep the script's wording without the `fm-herdr-lab-viewer:` prefix. Refused
//! sessions use Python-compatible `%r` quoting and Unicode 16.0.0 printability rules.
//!
//! The window size is set on the master before the fork, so the viewer's first grid read is
//! already non-zero. The viewer drops the inherited `HERDR_*` variables, because Herdr refuses to
//! launch a nested viewer from one of its own panes. The pidfile records the launcher and viewer
//! PIDs with their start times, which the lab helper checks before it signals anything.
//!
//! Exit status: the viewer's own status, or 0 when a signal killed it; 2 for a refused session or
//! pidfile; 3 when the pty, the fork, or the pidfile could not be set up; 7 on Windows.

use std::fmt;
use std::path::Path;

/// Every lab session name starts with this prefix. `default` and any other name are refused.
const SESSION_PREFIX: &str = "fm-lab-";

// DPES-864: These Unicode 16.0.0 ranges match Python 3.14.6 str.isprintable().
#[allow(clippy::useless_concat)] // Keep the generated table split across readable source lines.
const PYTHON_NONPRINTABLE_RANGES: &str = concat!(
    "00000000001F00007F0000A00000AD0000AD00037800037900038000038300038B00038B00038D00038D0003A20003A200053000053000055700055800058B00058C0005900005900005C80005CF0005EB0005EE0005F500060500061C00061C0006DD0006DD00070E00070F00074B00074C0007B20007BF0007FB0007FC00082E00082F00083F00083F00085C00085D00085F00085F00086B00086F00088F0008960008E20008E200098400098400098D00098E0009910009920009A90009A90009B10009B10009B30009B50009BA0009BB0009C50009C60009C90009CA0009CF0009D60009D80009DB0009DE0009DE0009E40009E50009FF000A00000A04000A04000A0B000A0E000A11000A12000A29000A29000A31000A31000A34000A34000A37000A37000A3A000A3B000A3D000A3D000A43000A46000A49000A4A000A4E000A50000A52000A58000A5D000A5D000A5F000A65000A77000A80000A84000A84000A8E000A8E000A92000A92000AA9000AA9000AB1000AB1000AB4000AB4000ABA000ABB000AC6000AC6000ACA000ACA000ACE000ACF000AD1000ADF000AE4000AE5000AF2000AF8000B00000B00000B04000B04000B0D000B0E000B11000B12000B29000B29000B31000B31000B34000B34000B3A000B3B000B45000B46000B49000B4A000B4E000B54000B58000B5B000B5E000B5E000B64000B65000B78000B81000B84000B84000B8B000B8D000B91000B91000B96000B98000B9B000B9B000B9D000B9D000BA0000BA2000BA5000BA7000BAB000BAD000BBA000BBD000BC3000BC5000BC9000BC9000BCE000BCF000BD1000BD6000BD8000BE5000BFB000BFF000C0D000C0D000C11000C11000C29000C29000C3A000C3B000C45000C45000C49000C49000C4E000C54000C57000C57000C5B000C5C000C5E000C5F000C64000C65000C70000C76000C8D000C8D000C91000C91000CA9000CA9000CB4000CB4000CBA000CBB000CC5000CC5000CC9000CC9000CCE000CD4000CD7000CDC000CDF000CDF000CE4000CE5000CF0000CF0000CF4000CFF000D0D000D0D000D11000D11000D45000D45000D49000D49000D50000D53000D64000D65000D80000D80000D84000D84000D97000D99000DB2000DB2000DBC000DBC000DBE000DBF000DC7000DC9000DCB000DCE000DD5000DD5000DD7000DD7000DE0000DE5000DF0000DF1000DF5000E00000E3B000E3E000E5C000E80000E83000E83000E85000E85000E8B000E8B000EA4000EA4000EA6000EA6000EBE000EBF000EC5000EC5000EC7000EC7000ECF000ECF000EDA000EDB000EE0000EFF000F48000F48000F6D000F70000F98000F98000FBD000FBD000FCD000FCD000FDB000FFF0010C60010C60010C80010CC0010CE0010CF00124900124900124E00124F00125700125700125900125900125E00125F00128900128900128E00128F0012B10012B10012B60012B70012BF0012BF0012C10012C10012C60012C70012D70012D700131100131100131600131700135B00135C00137D00137F00139A00139F0013F60013F70013FE0013FF00168000168000169D00169F0016F90016FF00171600171E00173700173F00175400175F00176D00176D00177100177100177400177F0017DE0017DF0017EA0017EF0017FA0017FF00180E00180E00181A00181F00187900187F0018AB0018AF0018F60018FF00191F00191F00192C00192F00193C00193F00194100194300196E00196F00197500197F0019AC0019AF0019CA0019CF0019DB0019DD001A1C001A1D001A5F001A5F001A7D001A7E001A8A001A8F001A9A001A9F001AAE001AAF001ACF001AFF001B4D001B4D001BF4001BFB001C38001C3A001C4A001C4C001C8B001C8F001CBB001CBC001CC8001CCF001CFB001CFF001F16001F17001F1E001F1F001F46001F47001F4E001F4F001F58001F58001F5A001F5A001F5C001F5C001F5E001F5E001F7E001F7F001FB5001FB5001FC5001FC5001FD4001FD5001FDC001FDC001FF0001FF1001FF5001FF5001FFF00200F00202800202F00205F00206F00207200207300208F00208F00209D00209F0020C10020CF0020F10020FF00218C00218F00242A00243F00244B00245F002B74002B75002B96002B96002CF4002CF8002D26002D26002D28002D2C002D2E002D2F002D68002D6E002D71002D7E002D97002D9F002DA7002DA7002DAF002DAF002DB7002DB7002DBF002DBF002DC7002DC7002DCF002DCF002DD7002DD7002DDF002DDF002E5E002E7F002E9A002E9A002EF4002EFF002FD6002FEF00300000300000304000304000309700309800310000310400313000313000318F00318F0031E60031EE00321F00321F00A48D00A48F00A4C700A4CF00A62C00A63F00A6F800A6FF00A7CE00A7CF00A7D200A7D200A7D400A7D400A7DD00A7F100A82D00A82F00A83A00A83F00A87800A87F00A8C600A8CD00A8DA00A8DF00A95400A95E00A97D00A97F00A9CE00A9CE00A9DA00A9DD00A9FF00A9FF00AA3700AA3F00AA4E00AA4F00AA5A00AA5B00AAC300AADA00AAF700AB0000AB0700AB0800AB0F00AB1000AB1700AB1F00AB2700AB2700AB2F00AB2F00AB6C00AB6F00ABEE00ABEF00ABFA00ABFF00D7A400D7AF00D7C700D7CA00D7FC00F8FF00FA6E00FA6F00FADA00FAFF00FB0700FB1200FB1800FB1C00FB3700FB3700FB3D00FB3D00FB3F00FB3F00FB4200FB4200FB4500FB4500FBC300FBD200FD9000FD9100FDC800FDCE00FDD000FDEF00FE1A00FE1F00FE5300FE5300FE6700FE6700FE6C00FE6F00FE7500FE7500FEFD00FF0000FFBF00FFC100FFC800FFC900FFD000FFD100FFD800FFD900FFDD00FFDF00FFE700FFE700FFEF00FFFB00FFFE00FFFF01000C01000C01002701002701003B01003B01003E01003E01004E01004F01005E01007F0100FB0100FF01010301010601013401013601018F01018F01019D01019F0101A10101CF0101FE01027F01029D01029F0102D10102DF0102FC0102FF01032401032C01034B01034F01037B01037F01039E01039E0103C40103C70103D60103FF01049E01049F0104AA0104AF0104D40104D70104FC0104FF01052801052F01056401056E01057B01057B01058B01058B0105930105930105960105960105A20105A20105B20105B20105BA0105BA0105BD0105BF0105F40105FF01073701073F01075601075F01076801077F0107860107860107B10107B10107BB0107FF01080601080701080901080901083601083601083901083B01083D01083E01085601085601089F0108A60108B00108DF0108F30108F30108F60108FA01091C01091E01093A01093E01094001097F0109B80109BB0109D00109D1010A04010A04010A07010A0B010A14010A14010A18010A18010A36010A37010A3B010A3E010A49010A4F010A59010A5F010AA0010ABF010AE7010AEA010AF7010AFF010B36010B38010B56010B57010B73010B77010B92010B98010B9D010BA8010BB0010BFF010C49010C7F010CB3010CBF010CF3010CF9010D28010D2F010D3A010D3F010D66010D68010D86010D8D010D90010E5F010E7F010E7F010EAA010EAA010EAE010EAF010EB2010EC1010EC5010EFB010F28010F2F010F5A010F6F010F8A010FAF010FCC010FDF010FF7010FFF01104E01105101107601107E0110BD0110BD0110C30110CF0110E90110EF0110FA0110FF01113501113501114801114F01117701117F0111E00111E00111F50111FF01121201121201124201127F01128701128701128901128901128E01128E01129E01129E0112AA0112AF0112EB0112EF0112FA0112FF01130401130401130D01130E01131101131201132901132901133101133101133401133401133A01133A01134501134601134901134A01134E01134F01135101135601135801135C01136401136501136D01136F01137501137F01138A01138A01138C01138D01138F01138F0113B60113B60113C10113C10113C30113C40113C60113C60113CB0113CB0113D60113D60113D90113E00113E30113FF01145C01145C01146201147F0114C80114CF0114DA01157F0115B60115B70115DE0115FF01164501164F01165A01165F01166D01167F0116BA0116BF0116CA0116CF0116E40116FF01171B01171C01172C01172F0117470117FF01183C01189F0118F30118FE01190701190801190A01190B01191401191401191701191701193601193601193901193A01194701194F01195A01199F0119A80119A90119D80119D90119E50119FF011A48011A4F011AA3011AAF011AF9011AFF011B0A011BBF011BE2011BEF011BFA011BFF011C09011C09011C37011C37011C46011C4F011C6D011C6F011C90011C91011CA8011CA8011CB7011CFF011D07011D07011D0A011D0A011D37011D39011D3B011D3B011D3E011D3E011D48011D4F011D5A011D5F011D66011D66011D69011D69011D8F011D8F011D92011D92011D99011D9F011DAA011EDF011EF9011EFF011F11011F11011F3B011F3D011F5B011FAF011FB1011FBF011FF2011FFE01239A0123FF01246F01246F01247501247F012544012F8F012FF3012FFF01343001343F01345601345F0143FB0143FF0146470160FF01613A0167FF016A39016A3F016A5F016A5F016A6A016A6D016ABF016ABF016ACA016ACF016AEE016AEF016AF6016AFF016B46016B4F016B5A016B5A016B62016B62016B78016B7C016B90016D3F016D7A016E3F016E9B016EFF016F4B016F4E016F88016F8E016FA0016FDF016FE5016FEF016FF2016FFF0187F80187FF018CD6018CFE018D0901AFEF01AFF401AFF401AFFC01AFFC01AFFF01AFFF01B12301B13101B13301B14F01B15301B15401B15601B16301B16801B16F01B2FC01BBFF01BC6B01BC6F01BC7D01BC7F01BC8901BC8F01BC9A01BC9B01BCA001CBFF01CCFA01CCFF01CEB401CEFF01CF2E01CF2F01CF4701CF4F01CFC401CFFF01D0F601D0FF01D12701D12801D17301D17A01D1EB01D1FF01D24601D2BF01D2D401D2DF01D2F401D2FF01D35701D35F01D37901D3FF01D45501D45501D49D01D49D01D4A001D4A101D4A301D4A401D4A701D4A801D4AD01D4AD01D4BA01D4BA01D4BC01D4BC01D4C401D4C401D50601D50601D50B01D50C01D51501D51501D51D01D51D01D53A01D53A01D53F01D53F01D54501D54501D54701D54901D55101D55101D6A601D6A701D7CC01D7CD01DA8C01DA9A01DAA001DAA001DAB001DEFF01DF1F01DF2401DF2B01DFFF01E00701E00701E01901E01A01E02201E02201E02501E02501E02B01E02F01E06E01E08E01E09001E0FF01E12D01E12F01E13E01E13F01E14A01E14D01E15001E28F01E2AF01E2BF01E2FA01E2FE01E30001E4CF01E4FA01E5CF01E5FB01E5FE01E60001E7DF01E7E701E7E701E7EC01E7EC01E7EF01E7EF01E7FF01E7FF01E8C501E8C601E8D701E8FF01E94C01E94F01E95A01E95D01E96001EC7001ECB501ED0001ED3E01EDFF01EE0401EE0401EE2001EE2001EE2301EE2301EE2501EE2601EE2801EE2801EE3301EE3301EE3801EE3801EE3A01EE3A01EE3C01EE4101EE4301EE4601EE4801EE4801EE4A01EE4A01EE4C01EE4C01EE5001EE5001EE5301EE5301EE5501EE5601EE5801EE5801EE5A01EE5A01EE5C01EE5C01EE5E01EE5E01EE6001EE6001EE6301EE6301EE6501EE6601EE6B01EE6B01EE7301EE7301EE7801EE7801EE7D01EE7D01EE7F01EE7F01EE8A01EE8A01EE9C01EEA001EEA401EEA401EEAA01EEAA01EEBC01EEEF01EEF201EFFF01F02C01F02F01F09401F09F01F0AF01F0B001F0C001F0C001F0D001F0D001F0F601F0FF01F1AE01F1E501F20301F20F01F23C01F23F01F24901F24F01F25201F25F01F26601F2FF01F6D801F6DB01F6ED01F6EF01F6FD01F6FF01F77701F77A01F7DA01F7DF01F7EC01F7EF01F7F101F7FF01F80C01F80F01F84801F84F01F85A01F85F01F88801F88F01F8AE01F8AF01F8BC01F8BF01F8C201F8FF01FA5401FA5F01FA6E01FA6F01FA7D01FA7F01FA8A01FA8E01FAC701FACD01FADD01FADE01FAEA01FAEF01FAF901FAFF01FB9301FB9301FBFA01FFFF02A6E002A6FF02B73A02B73F02B81E02B81F02CEA202CEAF02EBE102EBEF02EE5E02F7FF02FA1E02FFFF03134B03134F0323B00E00FF0E01F010FFFF",
);

/// Why `mcao herdr lab-viewer` refused or could not attach.
#[derive(Debug)]
pub enum LabViewerError {
    /// The session or pidfile argument is refused. Exit code 2.
    Usage(String),
    /// The pty, the viewer process, or the pidfile could not be set up. Exit code 3.
    #[cfg(unix)]
    Spawn(String),
    /// The platform has no pty viewer. Exit code 7.
    #[cfg(not(unix))]
    Unsupported,
}

impl LabViewerError {
    /// The process exit code for this failure.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            #[cfg(unix)]
            Self::Spawn(_) => 3,
            #[cfg(not(unix))]
            Self::Unsupported => 7,
        }
    }
}

impl fmt::Display for LabViewerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            #[cfg(unix)]
            Self::Spawn(message) => f.write_str(message),
            #[cfg(not(unix))]
            Self::Unsupported => {
                f.write_str("Unsupported: herdr does not support lab-viewer on this platform")
            }
        }
    }
}

fn unicode_range_endpoint(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => (byte - b'0') as u32,
            b'A'..=b'F' => (byte - b'A' + 10) as u32,
            _ => unreachable!("the static Python Unicode ranges contain only hex digits"),
        };
        (value << 4) | digit
    })
}

fn python_is_printable(character: char) -> bool {
    if character.is_ascii() {
        return (' '..='~').contains(&character);
    }

    let codepoint = character as u32;
    let ranges = PYTHON_NONPRINTABLE_RANGES.as_bytes();
    let mut low = 0;
    let mut high = ranges.len() / 12;
    while low < high {
        let middle = low + (high - low) / 2;
        let offset = middle * 12;
        let start = unicode_range_endpoint(&ranges[offset..offset + 6]);
        if codepoint < start {
            high = middle;
            continue;
        }
        let end = unicode_range_endpoint(&ranges[offset + 6..offset + 12]);
        if codepoint <= end {
            return false;
        }
        low = middle + 1;
    }
    true
}

fn python_repr(value: &str) -> String {
    use std::fmt::Write as _;

    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut repr = String::with_capacity(value.len() + 2);
    repr.push(quote);
    for character in value.chars() {
        match character {
            '\\' => repr.push_str("\\\\"),
            '\t' => repr.push_str("\\t"),
            '\n' => repr.push_str("\\n"),
            '\r' => repr.push_str("\\r"),
            character if character == quote => {
                repr.push('\\');
                repr.push(character);
            }
            character if python_is_printable(character) => repr.push(character),
            character if character as u32 <= 0xff => {
                write!(repr, "\\x{:02x}", character as u32).unwrap();
            }
            character if character as u32 <= 0xffff => {
                write!(repr, "\\u{:04x}", character as u32).unwrap();
            }
            character => {
                write!(repr, "\\U{:08x}", character as u32).unwrap();
            }
        }
    }
    repr.push(quote);
    repr
}

/// Accepts a name the way the reference's `fm-lab-[A-Za-z0-9][A-Za-z0-9_-]*` pattern does.
pub fn validate_session(session: &str) -> Result<(), LabViewerError> {
    let accepted = session.strip_prefix(SESSION_PREFIX).is_some_and(|rest| {
        let mut chars = rest.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    });
    if accepted {
        Ok(())
    } else {
        Err(LabViewerError::Usage(format!(
            "refusing session {}",
            python_repr(session)
        )))
    }
}

/// The launcher records its identity at this path for the lab helper, so it must be absolute.
pub fn validate_pidfile(pidfile: &Path) -> Result<(), LabViewerError> {
    if pidfile.is_absolute() {
        Ok(())
    } else {
        Err(LabViewerError::Usage(
            "pidfile must be an absolute path".to_owned(),
        ))
    }
}

/// Checks both arguments before any pty exists. The session is checked first, as the reference does.
pub fn validate(session: &str, pidfile: &Path) -> Result<(), LabViewerError> {
    validate_session(session)?;
    validate_pidfile(pidfile)
}

/// Attaches the viewer, waits for it, and returns the status this process exits with.
pub fn run(session: &str, pidfile: &Path) -> Result<u8, LabViewerError> {
    validate(session, pidfile)?;
    attach(session, pidfile)
}

#[cfg(unix)]
use pty::attach;

#[cfg(not(unix))]
fn attach(_session: &str, _pidfile: &Path) -> Result<u8, LabViewerError> {
    Err(LabViewerError::Unsupported)
}

#[cfg(unix)]
mod pty {
    use std::fs;
    use std::io::{self, ErrorKind};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    use super::LabViewerError;

    /// Herdr inherits these from the pane this runs in, and it refuses a nested viewer outright.
    const SCRUBBED_ENV: [&str; 7] = [
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_TAB_ID",
        "HERDR_WORKSPACE_ID",
        "HERDR_SOCKET_PATH",
        "HERDR_BIN_PATH",
        "HERDR_SESSION",
    ];
    const TERMINATION_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];
    const ROWS: u16 = 40;
    const COLS: u16 = 120;
    const TERMINATE_GRACE_SECONDS: libc::time_t = 5;
    const READ_CHUNK: usize = 65536;

    /// The viewer's pid for the signal handlers. Zero until the fork has returned.
    static VIEWER_PID: AtomicI32 = AtomicI32::new(0);

    /// Set just before the pidfile is renamed into place. Until then a termination signal cancels
    /// the launch; after it, the signal is relayed to the viewer.
    static RECORDED: AtomicBool = AtomicBool::new(false);

    /// Owns a descriptor and closes it on drop.
    struct Fd(libc::c_int);

    impl Drop for Fd {
        fn drop(&mut self) {
            // SAFETY: the descriptor was returned by openpty and is closed only here.
            unsafe {
                libc::close(self.0);
            }
        }
    }

    fn spawn_error(context: &str, error: &io::Error) -> LabViewerError {
        LabViewerError::Spawn(format!("{context}: {error}"))
    }

    fn open_pty() -> io::Result<(Fd, Fd)> {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: openpty writes the two descriptors through the pointers; the name and
        // terminal settings may be null.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let (master, slave) = (Fd(master), Fd(slave));
        let window = libc::winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads one winsize struct from the pointer.
        let rc = unsafe { libc::ioctl(master.0, libc::TIOCSWINSZ as _, &window as *const _) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((master, slave))
    }

    /// The viewer command, built before the fork so the child only execs it. `TERM` defaults to
    /// `xterm-256color` when unset.
    fn viewer_command(session: &str) -> Command {
        let mut command = Command::new("herdr");
        command.arg("--session").arg(session);
        for name in SCRUBBED_ENV {
            command.env_remove(name);
        }
        if std::env::var_os("TERM").is_none() {
            command.env("TERM", "xterm-256color");
        }
        command
    }

    fn termination_set() -> libc::sigset_t {
        // SAFETY: sigemptyset and sigaddset only write the set they are given.
        unsafe {
            let mut set = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut set);
            for signal in TERMINATION_SIGNALS {
                libc::sigaddset(&mut set, signal);
            }
            set
        }
    }

    /// Blocks or unblocks the termination signals, which pthread_sigmask reports as an errno.
    fn set_termination_mask(how: libc::c_int) -> io::Result<()> {
        let set = termination_set();
        // SAFETY: pthread_sigmask reads the set and no old mask is requested.
        let rc = unsafe { libc::pthread_sigmask(how, &set, std::ptr::null_mut()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc))
        }
    }

    fn install(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) -> io::Result<()> {
        // SAFETY: sigaction reads the action struct. SA_RESTART is off, so a blocked read or wait
        // returns EINTR and the loops below retry it.
        unsafe {
            let mut action = std::mem::zeroed::<libc::sigaction>();
            action.sa_sigaction = handler as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn install_termination(handler: extern "C" fn(libc::c_int)) -> io::Result<()> {
        TERMINATION_SIGNALS
            .into_iter()
            .try_for_each(|signal| install(signal, handler))
    }

    fn kill_viewer(signal: libc::c_int) {
        let pid = VIEWER_PID.load(Ordering::SeqCst);
        if pid > 0 {
            // SAFETY: kill is async-signal-safe; a viewer that already exited yields ESRCH, ignored.
            unsafe {
                libc::kill(pid, signal);
            }
        }
    }

    fn set_grace_timer(seconds: libc::time_t) {
        let timer = libc::itimerval {
            it_interval: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            it_value: libc::timeval {
                tv_sec: seconds,
                tv_usec: 0,
            },
        };
        // SAFETY: setitimer reads the timer struct and is async-signal-safe.
        unsafe {
            libc::setitimer(libc::ITIMER_REAL, &timer, std::ptr::null_mut());
        }
    }

    /// Before the pidfile exists, a termination signal kills the viewer and exits 128 + signal.
    fn cancel_before_record(signal: libc::c_int) -> ! {
        kill_viewer(libc::SIGKILL);
        // SAFETY: _exit is async-signal-safe and skips the cleanup this launcher has no use for.
        unsafe { libc::_exit(128 + signal) }
    }

    /// Installed before the pidfile is written, so the relay is armed by the time the pidfile
    /// exists. Once the pidfile is recorded, a termination signal asks the viewer to exit.
    extern "C" fn on_termination(signal: libc::c_int) {
        if RECORDED.load(Ordering::SeqCst) {
            request_viewer_exit();
        } else {
            cancel_before_record(signal);
        }
    }

    /// Sends SIGTERM to the viewer and arms the grace timer that escalates to SIGKILL.
    fn request_viewer_exit() {
        kill_viewer(libc::SIGTERM);
        set_grace_timer(TERMINATE_GRACE_SECONDS);
    }

    extern "C" fn kill_after_grace(_signal: libc::c_int) {
        kill_viewer(libc::SIGKILL);
    }

    /// Runs in the forked child and never returns: it execs Herdr, or exits 127 if the exec fails.
    fn exec_child(slave: libc::c_int, master: libc::c_int, command: &mut Command) -> ! {
        // The parent blocked these before the fork, and the viewer must receive them.
        let _ = set_termination_mask(libc::SIG_UNBLOCK);
        // SAFETY: these calls act only on descriptors this child inherited from openpty.
        unsafe {
            libc::setsid();
            // Failure is ignored, as the reference ignores it.
            libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
            for target in 0..=2 {
                libc::dup2(slave, target);
            }
            if slave > 2 {
                libc::close(slave);
            }
            libc::close(master);
        }
        let _ = command.exec();
        // SAFETY: _exit ends the child without running the parent's cleanup.
        unsafe { libc::_exit(127) }
    }

    fn process_start(pid: u32) -> io::Result<String> {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .env("LC_ALL", "C")
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("ps could not report the process"));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let value = text.trim();
        if value.is_empty() {
            return Err(io::Error::other("process start time unavailable"));
        }
        Ok(value.to_owned())
    }

    /// Writes the launcher and viewer PIDs with their start times, then renames the file into place.
    /// The relay is armed before the rename, so a pidfile that exists means signals are relayed.
    fn write_pidfile(path: &Path, launcher_pid: u32, viewer_pid: u32) -> io::Result<()> {
        let launcher_start = process_start(launcher_pid)?;
        let viewer_start = process_start(viewer_pid)?;
        let contents = format!(
            "launcher_pid={launcher_pid}\nlauncher_start={launcher_start}\nviewer_pid={viewer_pid}\nviewer_start={viewer_start}\n"
        );
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(format!(".{launcher_pid}.tmp"));
        fs::write(&temporary, contents)?;
        RECORDED.store(true, Ordering::SeqCst);
        fs::rename(&temporary, path)
    }

    /// Reads and discards pty output until the viewer closes its end. Signals interrupt the read,
    /// and the read is retried.
    fn drain(master: libc::c_int) {
        let mut buffer = vec![0u8; READ_CHUNK];
        loop {
            // SAFETY: read writes at most buffer.len() bytes into the buffer.
            let read = unsafe { libc::read(master, buffer.as_mut_ptr().cast(), buffer.len()) };
            if read > 0 {
                continue;
            }
            if read == 0 || io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                return;
            }
        }
    }

    /// Waits for the viewer and returns its raw status. A wait that fails for any reason other than
    /// EINTR yields 0, as the reference does.
    fn wait_for(pid: libc::pid_t) -> libc::c_int {
        let mut status: libc::c_int = 0;
        loop {
            // SAFETY: waitpid writes the status through a valid pointer.
            let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
            if rc >= 0 {
                return status;
            }
            if io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                return 0;
            }
        }
    }

    /// Kills and reaps a viewer this launcher could not record. Returns the error to report.
    fn abandon(pid: libc::pid_t, error: LabViewerError) -> LabViewerError {
        VIEWER_PID.store(0, Ordering::SeqCst);
        // SAFETY: kill takes the pid of the viewer this launcher forked and has not reaped.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        wait_for(pid);
        error
    }

    /// Maps the viewer's wait status to this process's exit status: 0 when a signal killed the
    /// viewer, otherwise the viewer's exit code.
    fn exit_status(status: libc::c_int) -> u8 {
        if libc::WIFSIGNALED(status) {
            0
        } else {
            libc::WEXITSTATUS(status) as u8
        }
    }

    pub fn attach(session: &str, pidfile: &Path) -> Result<u8, LabViewerError> {
        let (master, slave) =
            open_pty().map_err(|error| spawn_error("could not create a pty", &error))?;
        let mut command = viewer_command(session);

        // Blocked until the handlers are installed, so an early signal is held rather than lost.
        set_termination_mask(libc::SIG_BLOCK)
            .map_err(|error| spawn_error("could not block signals", &error))?;
        // SAFETY: fork has no preconditions. The child execs at once and touches no shared state.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let error = io::Error::last_os_error();
            let _ = set_termination_mask(libc::SIG_UNBLOCK);
            return Err(spawn_error("could not fork the viewer", &error));
        }
        if pid == 0 {
            exec_child(slave.0, master.0, &mut command);
        }

        VIEWER_PID.store(pid, Ordering::SeqCst);
        let handlers = install_termination(on_termination)
            .and_then(|()| install(libc::SIGALRM, kill_after_grace))
            .and_then(|()| set_termination_mask(libc::SIG_UNBLOCK));
        if let Err(error) = handlers {
            return Err(abandon(
                pid,
                spawn_error("could not install signal handlers", &error),
            ));
        }

        drop(slave);
        let launcher_pid = std::process::id();
        if let Err(error) = write_pidfile(pidfile, launcher_pid, pid as u32) {
            return Err(abandon(
                pid,
                spawn_error("could not record process identity", &error),
            ));
        }

        drain(master.0);
        request_viewer_exit();
        let status = wait_for(pid);
        VIEWER_PID.store(0, Ordering::SeqCst);
        set_grace_timer(0);
        Ok(exit_status(status))
    }
}
