//! Scanner for the OSC sequences `alacritty_terminal` 0.26 does not surface:
//! OSC 7 (current working directory) and OSC 133 (shell prompt / command marks).
//!
//! The scanner watches the raw PTY byte stream *alongside* the Alacritty parser. It
//! never alters bytes; it reports each event with the byte offset just past its
//! terminator so the caller can feed the parser up to that point and then read the
//! grid cursor position the event applies to.
use std::path::PathBuf;

const MAX_PAYLOAD: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OscEvent {
    /// OSC 7: the shell's working directory on this host.
    Cwd(PathBuf),
    /// OSC 133;A: a prompt is about to be drawn.
    PromptStart,
    /// OSC 133;C: the command was submitted and output begins.
    OutputStart,
    /// OSC 133;D[;code]: the command finished.
    CommandEnd(Option<i32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Osc,
    OscEsc,
}

#[derive(Debug)]
pub struct OscScanner {
    state: State,
    payload: Vec<u8>,
    overflow: bool,
    host: String,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self::new(local_hostname())
    }
}

impl OscScanner {
    /// `host` is this machine's name; OSC 7 URIs naming other hosts (for example from
    /// an SSH session) are ignored so a remote path is never used as a local cwd.
    pub fn new(host: String) -> Self {
        Self {
            state: State::Ground,
            payload: Vec::new(),
            overflow: false,
            host,
        }
    }

    /// Feed a chunk; state carries across chunks so split sequences are handled.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<(usize, OscEvent)> {
        let mut events = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                    }
                }
                State::Esc => match b {
                    b']' => self.begin(),
                    _ if esc_stays(b) => {}
                    _ => self.state = State::Ground,
                },
                State::Osc => match b {
                    0x07 => self.finish(i + 1, &mut events),
                    0x1b => self.state = State::OscEsc,
                    0x18 | 0x1a => self.state = State::Ground,
                    _ => {
                        if self.payload.len() < MAX_PAYLOAD {
                            self.payload.push(b);
                        } else {
                            self.overflow = true;
                        }
                    }
                },
                State::OscEsc => match b {
                    b'\\' => self.finish(i + 1, &mut events),
                    b']' => self.begin(),
                    _ if esc_stays(b) => {}
                    _ => self.state = State::Ground,
                },
            }
        }
        events
    }

    fn begin(&mut self) {
        self.state = State::Osc;
        self.payload.clear();
        self.overflow = false;
    }

    fn finish(&mut self, end: usize, events: &mut Vec<(usize, OscEvent)>) {
        self.state = State::Ground;
        if self.overflow {
            return;
        }
        if let Some(event) = parse(&self.payload, &self.host) {
            events.push((end, event));
        }
    }
}

/// Longest OSC string (bytes after `ESC ]`) handed to the terminal parser. The vte 0.15
/// parser Alacritty uses grows its OSC buffer without bound, so a program (or a stray
/// `cat` of a hostile file) could make it allocate arbitrarily much; this caps that.
/// It is above the base64 form of the largest honoured OSC 52 write (1 MiB), so a write
/// cut off here still decodes to more than the clipboard cap and is refused as too big.
///
/// Only OSC 52 (clipboard) may be that long. Every other OSC is capped at
/// [`MAX_OSC_OTHER_BYTES`]: Alacritty clones an OSC 2 title into a 4096-entry stack on
/// each `CSI 22;0t`, so a megabyte title there cost hundreds of MB of memory.
pub const MAX_OSC_BYTES: usize = 1024 * 1024 * 4 / 3 + 8 * 1024;

/// Cap for every OSC string except OSC 52: 8 KiB. That is twice [`MAX_PAYLOAD`] (the
/// longest OSC 7/133 the scanner honours), so the guard never truncates a string the
/// scanner would accept, and well above the 2083 bytes terminals promise OSC 8
/// hyperlink URIs (plus `id=` params) and above any real title, palette or notification
/// string. A title stack full of capped titles stays under 4096 * 8 KiB = 32 MiB.
pub const MAX_OSC_OTHER_BYTES: usize = 8 * 1024;

/// How much of the OSC number (the bytes before the first `;`) has been seen. Only the
/// exact bytes `52;` earn the large cap, as in vte, which matches `params[0] == b"52"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Number {
    Start,
    Five,
    FiftyTwo,
    /// Past the number, and it is not 52.
    Other,
    /// `52;` seen.
    Clipboard,
}

impl Number {
    fn next(self, b: u8) -> Self {
        match (self, b) {
            (Self::Start, b'5') => Self::Five,
            (Self::Five, b'2') => Self::FiftyTwo,
            (Self::FiftyTwo, b';') => Self::Clipboard,
            (Self::Clipboard, _) => Self::Clipboard,
            _ => Self::Other,
        }
    }
    fn cap(self) -> usize {
        if self == Self::Clipboard {
            MAX_OSC_BYTES
        } else {
            MAX_OSC_OTHER_BYTES
        }
    }
}

/// Bytes that leave vte 0.15's Escape state unchanged: it executes the C0 controls
/// (except CAN/SUB, which return to Ground), ignores DEL and anything >= 0x80 (it has
/// no 8-bit C1 handling there), and ESC restarts the escape. So `ESC NUL ]` and
/// `ESC DEL ]` still begin an OSC string, exactly like `ESC ]`.
fn esc_stays(b: u8) -> bool {
    matches!(b, 0x00..=0x17 | 0x19 | 0x1b..=0x1f | 0x7f..=0xff)
}

/// Mirror of the vte 0.15 states that can lead into an OSC string. Every other vte
/// state (CSI, DCS, SOS/PM/APC, escape intermediates, UTF-8 text) behaves like
/// `Ground` here: it can only reach an OSC through `ESC` and then `]` from Escape, and
/// vte does not buffer DCS/SOS/PM/APC payload, so those need no bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuardState {
    Ground,
    Esc,
    /// Bytes of the string so far, and what its number says about its cap.
    Osc(usize, Number),
    /// Over the cap: payload bytes are dropped until the string ends.
    Skip,
}

/// Byte filter in front of the terminal parser that bounds OSC strings to
/// [`MAX_OSC_OTHER_BYTES`], or [`MAX_OSC_BYTES`] for OSC 52. Longer strings are cancelled (`CAN`) at the cap, which makes the
/// parser dispatch the truncated string, and the rest is dropped up to the terminator.
#[derive(Debug)]
pub struct OscGuard {
    state: GuardState,
}

impl Default for OscGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl OscGuard {
    pub fn new() -> Self {
        Self {
            state: GuardState::Ground,
        }
    }

    /// `bytes` unchanged unless an OSC string in it passes the cap.
    pub fn filter<'a>(&mut self, bytes: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        use GuardState::*;
        let mut out: Option<Vec<u8>> = None;
        for (i, &b) in bytes.iter().enumerate() {
            let mut keep = true;
            let mut cancel = false;
            self.state = match (self.state, b) {
                (Ground, 0x1b) => Esc,
                (Ground, _) => Ground,
                (Esc, b']') => Osc(0, Number::Start),
                (Esc, b) if esc_stays(b) => Esc,
                (Esc, _) => Ground,
                (Osc(..), 0x07 | 0x18 | 0x1a) => Ground,
                // vte ends the string at ESC and is then in Escape, not only for `\\`.
                (Osc(..), 0x1b) => Esc,
                (Osc(n, num), _) if n >= num.cap() => {
                    cancel = true;
                    keep = false;
                    Skip
                }
                (Osc(n, num), b) => Osc(n + 1, num.next(b)),
                (Skip, 0x07) => {
                    keep = false;
                    Ground
                }
                (Skip, 0x18 | 0x1a) => Ground,
                (Skip, 0x1b) => Esc,
                (Skip, _) => {
                    keep = false;
                    Skip
                }
            };
            if (!keep || cancel) && out.is_none() {
                out = Some(bytes[..i].to_vec());
            }
            if let Some(out) = out.as_mut() {
                if cancel {
                    out.push(0x18);
                } else if keep {
                    out.push(b);
                }
            }
        }
        match out {
            Some(v) => v.into(),
            None => bytes.into(),
        }
    }
}

fn parse(payload: &[u8], host: &str) -> Option<OscEvent> {
    let text = std::str::from_utf8(payload).ok()?;
    let (code, rest) = text.split_once(';').unwrap_or((text, ""));
    match code {
        "7" => cwd_from_uri(rest, host).map(OscEvent::Cwd),
        "133" => {
            let mut parts = rest.split(';');
            match parts.next()? {
                "A" => Some(OscEvent::PromptStart),
                "C" => Some(OscEvent::OutputStart),
                "D" => Some(OscEvent::CommandEnd(
                    parts.next().and_then(|c| c.parse().ok()),
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `file://host/path` -> absolute local path, or `None` for other hosts / bad input.
pub fn cwd_from_uri(uri: &str, local_host: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    let host = host.to_ascii_lowercase();
    if !(host.is_empty()
        || host == "localhost"
        || (!local_host.is_empty() && host == local_host.to_ascii_lowercase()))
    {
        return None;
    }
    let decoded = percent_decode(path)?;
    if decoded.contains('\0') {
        return None;
    }
    Some(PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn local_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Vec<(usize, OscEvent)> {
        OscScanner::new("box".into()).scan(bytes)
    }

    #[test]
    fn parses_cwd_bel_and_st_terminators() {
        let a = scan(b"x\x1b]7;file://box/home/me/a%20b\x07y");
        assert_eq!(a, vec![(30, OscEvent::Cwd("/home/me/a b".into()))]);
        let b = scan(b"\x1b]7;file:///tmp\x1b\\");
        assert_eq!(b[0].1, OscEvent::Cwd("/tmp".into()));
        assert_eq!(b[0].0, 17);
    }

    #[test]
    fn ignores_foreign_hosts_and_bad_uris() {
        assert!(scan(b"\x1b]7;file://other.example/etc\x07").is_empty());
        assert!(scan(b"\x1b]7;http://box/etc\x07").is_empty());
        assert!(scan(b"\x1b]7;file://box/bad%zz\x07").is_empty());
        assert!(scan(b"\x1b]7;file://box/nul%00\x07").is_empty());
        assert!(scan(b"\x1b]7;file://box\x07").is_empty());
    }

    #[test]
    fn parses_prompt_marks() {
        let e: Vec<_> = scan(b"\x1b]133;D;3\x07\x1b]133;A\x07$ \x1b]133;C\x07\x1b]133;D\x07")
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        assert_eq!(
            e,
            vec![
                OscEvent::CommandEnd(Some(3)),
                OscEvent::PromptStart,
                OscEvent::OutputStart,
                OscEvent::CommandEnd(None),
            ]
        );
        // Extra parameters (aid=, cl=) after A are tolerated.
        assert_eq!(
            scan(b"\x1b]133;A;cl=m;aid=1\x07")[0].1,
            OscEvent::PromptStart
        );
    }

    #[test]
    fn sequences_split_across_chunks_are_reassembled() {
        let mut s = OscScanner::new("box".into());
        assert!(s.scan(b"abc\x1b]13").is_empty());
        assert!(s.scan(b"3;D;12").is_empty());
        let e = s.scan(b"7\x07rest");
        assert_eq!(e, vec![(2, OscEvent::CommandEnd(Some(127)))]);
        let mut s = OscScanner::new("box".into());
        assert!(s.scan(b"\x1b").is_empty());
        assert!(s.scan(b"]133;A\x1b").is_empty());
        assert_eq!(s.scan(b"\\").len(), 1);
    }

    #[test]
    fn encoded_hostile_directory_name_is_one_cwd_event() {
        // A directory named "evil BEL ESC]133;D;99 BEL" as the shell snippets encode it.
        let e = scan(b"\x1b]7;file://box/tmp/evil%07%1B%5D133%3BD%3B99%07%20%25\x07");
        assert_eq!(
            e.into_iter().map(|(_, e)| e).collect::<Vec<_>>(),
            vec![OscEvent::Cwd("/tmp/evil\x07\x1b]133;D;99\x07 %".into())]
        );
    }

    #[test]
    fn unrelated_and_hostile_input_is_harmless() {
        assert!(scan(b"\x1b]0;title\x07\x1b]8;;http://x\x07plain \x1b[31mred").is_empty());
        // Cancelled and oversized sequences produce nothing and the scanner recovers.
        let mut big = b"\x1b]133;".to_vec();
        big.extend(std::iter::repeat_n(b'A', MAX_PAYLOAD * 2));
        big.extend(b"\x07\x1b]133;A\x07");
        let e = scan(&big);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].1, OscEvent::PromptStart);
        assert!(scan(b"\x1b]133;A\x18\x07").is_empty());
    }

    #[test]
    fn guard_passes_normal_and_bounds_oversized_osc_strings() {
        let mut g = OscGuard::new();
        let normal = b"a\x1b]52;c;aGk=\x07b\x1b]0;t\x1b\\c";
        assert_eq!(&*g.filter(normal), &normal[..]);
        // 5 MiB OSC, fed in odd-sized chunks, followed by a good sequence.
        let mut stream = b"\x1b]52;c;".to_vec();
        stream.extend(std::iter::repeat_n(b'A', 5 * 1024 * 1024));
        stream.extend(b"\x07\x1b]133;A\x07tail");
        let mut out = Vec::new();
        for chunk in stream.chunks(7001) {
            out.extend_from_slice(&g.filter(chunk));
        }
        assert!(out.len() < MAX_OSC_BYTES + 64, "kept {} bytes", out.len());
        // The oversized string is cancelled, and what follows survives intact.
        assert!(out.windows(1).any(|w| w == [0x18]));
        assert!(
            out.ends_with(b"\x1b]133;A\x07tail"),
            "{:?}",
            &out[out.len() - 20..]
        );
        // ST-terminated overflow recovers too.
        let mut stream = b"\x1b]52;c;".to_vec();
        stream.extend(std::iter::repeat_n(b'A', MAX_OSC_BYTES + 100));
        stream.extend(b"\x1b\\ok");
        let out = g.filter(&stream).into_owned();
        assert!(out.ends_with(b"\x1b\\ok"));
        assert!(out.len() < MAX_OSC_BYTES + 64);
    }

    /// Longest OSC string (bytes between the params' `;`s excluded) vte dispatched.
    #[derive(Default)]
    struct Longest(usize, usize);
    impl alacritty_terminal::vte::Perform for Longest {
        fn osc_dispatch(&mut self, params: &[&[u8]], _bell: bool) {
            let len = params.iter().map(|p| p.len()).sum();
            self.0 = self.0.max(len);
            // Everything but OSC 52 (vte's `params[0] == b"52"`) has the small cap.
            if params.first() != Some(&&b"52"[..]) {
                self.1 = self.1.max(len);
            }
        }
    }
    /// What vte dispatches for `chunks` fed through a fresh guard; returns the longest
    /// dispatched OSC and the bytes the guard forwarded.
    fn through_guard(chunks: &[&[u8]]) -> (usize, Vec<u8>) {
        let (all, _, fwd) = through_guard_stats(chunks);
        (all, fwd)
    }
    /// As [`through_guard`], plus the longest dispatched OSC that is not OSC 52.
    fn through_guard_stats(chunks: &[&[u8]]) -> (usize, usize, Vec<u8>) {
        let (mut g, mut parser, mut seen) = (
            OscGuard::new(),
            alacritty_terminal::vte::Parser::new(),
            Longest::default(),
        );
        let mut fwd = Vec::new();
        for c in chunks {
            let f = g.filter(c);
            parser.advance(&mut seen, &f);
            fwd.extend_from_slice(&f);
        }
        (seen.0, seen.1, fwd)
    }
    fn long_osc(prefix: &[u8], len: usize, end: &[u8]) -> Vec<u8> {
        let mut v = prefix.to_vec();
        v.extend(b"52;c;");
        v.extend(std::iter::repeat_n(b'A', len));
        v.extend(end);
        v
    }
    /// Every way vte 0.15 can enter an OSC string from Escape, including controls it
    /// executes or ignores there (the guard used to fall back to Ground on them).
    #[test]
    fn guard_bounds_every_vte_route_into_an_osc_string() {
        let prefixes: [&[u8]; 9] = [
            b"\x1b]",
            b"\x1b\x00]",
            b"\x1b\x7f]",
            b"\x1b\x01]",
            b"\x1b\x1c]",
            b"\x1b\x1b]",
            b"\x1b\x80]",
            b"\x1b\x9d]",
            b"\x1b\x00\x7f\x1f\x1b\x19]",
        ];
        for prefix in prefixes {
            let stream = long_osc(prefix, 3 * 1024 * 1024, b"\x07tail");
            // Whole, and split at every position around the introducer.
            for split in 0..=prefix.len() + 6 {
                let (max, fwd) = through_guard(&[&stream[..split], &stream[split..]]);
                assert!(max <= MAX_OSC_BYTES, "{prefix:?} split {split}: {max}");
                assert!(fwd.len() < MAX_OSC_BYTES + 64 + prefix.len());
                assert!(fwd.ends_with(b"tail"), "{prefix:?} split {split}");
            }
            let chunks: Vec<&[u8]> = stream.chunks(7001).collect();
            assert!(through_guard(&chunks).0 <= MAX_OSC_BYTES, "{prefix:?}");
        }
    }
    #[test]
    fn guard_bounds_an_osc_started_right_after_another_ends_with_esc() {
        for mid in [&b"\x1b"[..], b"\x1b\x00", b"\x1b\x7f", b"\x1b\x1b"] {
            let mut stream = b"\x1b]0;title".to_vec();
            stream.extend(mid);
            stream.extend(long_osc(b"]", 3 * 1024 * 1024, b"\x1b\\ok").iter());
            let (max, fwd) = through_guard(&[&stream]);
            assert!(max <= MAX_OSC_BYTES, "{mid:?}: {max}");
            assert!(fwd.ends_with(b"\x1b\\ok"), "{mid:?}");
        }
    }
    #[test]
    fn guard_counts_interleaved_controls_and_resets_on_can_sub() {
        // Ignored C0 bytes inside the string still count toward the cap (conservative).
        let mut stream = b"\x1b]52;c;".to_vec();
        for _ in 0..(MAX_OSC_BYTES / 2 + 10) {
            stream.extend(b"A\x00");
        }
        stream.extend(b"\x07");
        assert!(through_guard(&[&stream]).0 <= MAX_OSC_BYTES);
        // CAN ends the string; what follows is plain text and passes untouched.
        let mut stream = long_osc(b"\x1b]", 1000, b"\x18");
        stream.extend(std::iter::repeat_n(b'B', 2 * MAX_OSC_BYTES));
        let (_, fwd) = through_guard(&[&stream]);
        assert_eq!(fwd, stream);
    }
    /// The large cap is for OSC 52 only: every other number is bounded to 8 KiB, the
    /// number may be split across reads, and what follows survives.
    #[test]
    fn guard_caps_every_non_clipboard_osc_small() {
        let body = |n: usize| std::iter::repeat_n(b'A', n);
        for num in [
            &b"0"[..],
            b"1",
            b"2",
            b"8",
            b"7",
            b"4",
            b"133",
            b"",
            b"5",
            b"520",
            b"052",
            b"52x",
            b"5 2",
            b"+52",
        ] {
            let mut stream = b"\x1b]".to_vec();
            stream.extend(num);
            stream.push(b';');
            stream.extend(body(1024 * 1024));
            stream.extend(b"\x07tail");
            // Whole, and split at every position through the number and its `;`.
            for split in 0..=num.len() + 4 {
                let (all, other, fwd) = through_guard_stats(&[&stream[..split], &stream[split..]]);
                assert!(
                    other <= MAX_OSC_OTHER_BYTES,
                    "{num:?} split {split}: {other}"
                );
                assert!(all <= MAX_OSC_OTHER_BYTES, "{num:?} split {split}: {all}");
                assert!(
                    fwd.len() < MAX_OSC_OTHER_BYTES + 64,
                    "{num:?}: {}",
                    fwd.len()
                );
                assert!(fwd.ends_with(b"tail"), "{num:?} split {split}");
            }
            // Byte-at-a-time reads give the same bytes as one read.
            let one: Vec<&[u8]> = stream.chunks(1).take(64).collect();
            let rest = &stream[one.len()..];
            let mut parts = one;
            parts.push(rest);
            assert_eq!(through_guard(&parts), through_guard(&[&stream]), "{num:?}");
        }
        // OSC 52 keeps its large cap, split anywhere in "52;" too.
        let stream = long_osc(b"\x1b]", 1024 * 1024, b"\x07tail");
        for split in 0..8 {
            let (all, other, fwd) = through_guard_stats(&[&stream[..split], &stream[split..]]);
            assert_eq!((all, other), (1024 * 1024 + 3, 0), "split {split}");
            assert_eq!(fwd, stream, "split {split}");
        }
    }
    /// Reviewer's case: a 1.4 MB OSC 2 title (what `CSI 22;0t` would then clone into
    /// Alacritty's title stack, once per push) reaches the parser as at most 8 KiB.
    #[test]
    fn guard_bounds_a_big_osc_2_title() {
        let mut stream = b"\x1b]2;".to_vec();
        stream.extend(std::iter::repeat_n(b'T', 1_400_000));
        stream.extend(b"\x07\x1b[22;0t");
        let (all, fwd) = through_guard(&[&stream]);
        assert!(all <= MAX_OSC_OTHER_BYTES, "dispatched {all} bytes");
        assert!(fwd.len() < MAX_OSC_OTHER_BYTES + 64);
        assert!(fwd.ends_with(b"\x1b[22;0t"));
        // Ordinary long values still pass: an OSC 8 URI of 4 KiB and an OSC 7 path.
        for num in ["8;id=x;", "7;"] {
            let mut ok = format!("\x1b]{num}").into_bytes();
            ok.extend(std::iter::repeat_n(b'u', 4096));
            ok.extend(b"\x07");
            assert_eq!(through_guard(&[&ok]).1, ok, "{num}");
        }
    }
    /// Not an OSC in vte (escape intermediate / CSI / DCS), so nothing to bound and
    /// nothing may be altered.
    #[test]
    fn guard_leaves_non_osc_long_payloads_byte_identical() {
        for prefix in [
            &b"\x1b ]"[..],
            b"\x1b[",
            b"\x1bP",
            b"\x1bX",
            b"\x1b^",
            b"\x1b_",
        ] {
            let stream = long_osc(prefix, MAX_OSC_BYTES + 5000, b"\x1b\\z");
            let (max, fwd) = through_guard(&[&stream]);
            assert_eq!(max, 0, "{prefix:?}");
            assert_eq!(fwd, stream, "{prefix:?}");
        }
        // A long but legitimate OSC under the cap is forwarded unchanged.
        let ok = long_osc(b"\x1b]", MAX_OSC_BYTES - 100, b"\x07text");
        assert_eq!(through_guard(&[&ok]).1, ok);
    }
    /// Seeded random streams of the bytes that steer vte's parser: no dispatched OSC
    /// may exceed the cap, and the output must not depend on how reads were split.
    #[test]
    fn guard_fuzz_matches_vte_state_machine() {
        const ALPHA: &[u8] = b"\x1b\x1b]]\\\x07\x18\x1a\x00\x01\x1c\x1f\x7f\x80\x9d\x9c \
            P X ^ _ [ ; 5 2 A";
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..40 {
            let mut stream = Vec::new();
            for _ in 0..(next() % 40 + 5) {
                match next() % 10 {
                    0 => {
                        // An introducer with random bytes between ESC and `]`, then a
                        // run that straddles the cap.
                        stream.push(0x1b);
                        for _ in 0..next() % 3 {
                            stream.push(ALPHA[(next() % ALPHA.len() as u64) as usize]);
                        }
                        stream.push(b']');
                        // Random OSC number prefix: 52; gets the large cap, others small.
                        let num: &[u8] = [&b""[..], b"52;", b"5;", b"2;", b"520;", b"052;", b"52"]
                            [(next() % 7) as usize];
                        stream.extend(num);
                        let n = MAX_OSC_BYTES as u64 - 50 + next() % 200;
                        stream.extend(std::iter::repeat_n(b'A', n as usize));
                    }
                    1 => stream.extend(std::iter::repeat_n(b'B', (next() % 3000) as usize)),
                    _ => stream.push(ALPHA[(next() % ALPHA.len() as u64) as usize]),
                }
            }
            let whole = through_guard(&[&stream]);
            assert!(whole.0 <= MAX_OSC_BYTES, "round {round}: {}", whole.0);
            let stats = through_guard_stats(&[&stream]);
            assert!(
                stats.1 <= MAX_OSC_OTHER_BYTES,
                "round {round}: non-52 OSC of {} bytes",
                stats.1
            );
            let mut cuts: Vec<usize> = (0..4)
                .map(|_| (next() % (stream.len() as u64 + 1)) as usize)
                .collect();
            cuts.sort_unstable();
            let parts: Vec<&[u8]> = std::iter::once(0)
                .chain(cuts.iter().copied())
                .zip(cuts.iter().copied().chain(std::iter::once(stream.len())))
                .map(|(a, b)| &stream[a..b])
                .collect();
            assert_eq!(
                through_guard(&parts),
                whole,
                "round {round} split dependent"
            );
        }
    }
}
