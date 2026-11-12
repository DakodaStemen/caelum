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
pub const MAX_OSC_BYTES: usize = 1024 * 1024 * 4 / 3 + 8 * 1024;

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
    Osc(usize),
    /// Over the cap: payload bytes are dropped until the string ends.
    Skip,
}

/// Byte filter in front of the terminal parser that bounds OSC strings to
/// [`MAX_OSC_BYTES`]. Longer strings are cancelled (`CAN`) at the cap, which makes the
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
                (Esc, b']') => Osc(0),
                (Esc, b) if esc_stays(b) => Esc,
                (Esc, _) => Ground,
                (Osc(_), 0x07 | 0x18 | 0x1a) => Ground,
                // vte ends the string at ESC and is then in Escape, not only for `\\`.
                (Osc(_), 0x1b) => Esc,
                (Osc(n), _) if n >= MAX_OSC_BYTES => {
                    cancel = true;
                    keep = false;
                    Skip
                }
                (Osc(n), _) => Osc(n + 1),
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
    struct Longest(usize);
    impl alacritty_terminal::vte::Perform for Longest {
        fn osc_dispatch(&mut self, params: &[&[u8]], _bell: bool) {
            self.0 = self.0.max(params.iter().map(|p| p.len()).sum());
        }
    }
    /// What vte dispatches for `chunks` fed through a fresh guard; returns the longest
    /// dispatched OSC and the bytes the guard forwarded.
    fn through_guard(chunks: &[&[u8]]) -> (usize, Vec<u8>) {
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
        (seen.0, fwd)
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
                        let n = MAX_OSC_BYTES as u64 - 50 + next() % 200;
                        stream.extend(std::iter::repeat_n(b'A', n as usize));
                    }
                    1 => stream.extend(std::iter::repeat_n(b'B', (next() % 3000) as usize)),
                    _ => stream.push(ALPHA[(next() % ALPHA.len() as u64) as usize]),
                }
            }
            let whole = through_guard(&[&stream]);
            assert!(whole.0 <= MAX_OSC_BYTES, "round {round}: {}", whole.0);
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
