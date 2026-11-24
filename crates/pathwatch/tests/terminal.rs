//! The dashboard in a real pseudo-terminal: what a person's terminal is left
//! like on every way out, and the interactions that only exist with a tty.
//!
//! Nothing here reaches the network. `demo` plays scripted data, and the one
//! live session probes this machine's loopback address only.
#![cfg(unix)]

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ALT_ENTER: &str = "\x1b[?1049h";
const ALT_LEAVE: &str = "\x1b[?1049l";
/// What asks a terminal to report clicks and the wheel, and what stops it.
const MOUSE_ON: &str = "\x1b[?1000h";
const MOUSE_OFF: &str = "\x1b[?1000l";
const WAIT: Duration = Duration::from_secs(20);

/// Whether a process that is not this test's child is still running. One
/// that has exited but has not been collected by its parent still has an
/// entry, and still answers a signal: that is over, not running. It happens
/// when the orphan's new parent never collects — `cargo` itself, when it is
/// the first process in a container.
fn still_running(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    // Linux says what state it is in: the field after the name in brackets.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat.rsplit(')').next().is_some_and(|rest| rest.trim_start().starts_with('Z')),
        Err(_) => true,
    }
}

fn last_os_error<T>() -> io::Result<T> {
    Err(io::Error::last_os_error())
}

/// Runs in the forked child before `exec`. Gives the dashboard what a shell
/// would: a controlling terminal, its own foreground process group, and a
/// parent in the same session that waits for it. Without that parent the
/// group is orphaned and the kernel discards `SIGTSTP`, so suspend could not
/// be tested. Only async-signal-safe calls are made.
fn under_a_stand_in_shell() -> io::Result<()> {
    // SAFETY: plain system calls on this process's own descriptors and ids,
    // between fork and exec, none of which allocates or takes a lock.
    unsafe {
        if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
            return last_os_error();
        }
        match libc::fork() {
            -1 => last_os_error(),
            0 => {
                // The dashboard-to-be: foreground group of the terminal.
                libc::setpgid(0, 0);
                libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                libc::tcsetpgrp(0, libc::getpid());
                libc::signal(libc::SIGTTOU, libc::SIG_DFL);
                Ok(())
            }
            pid => {
                // The stand-in shell: holds nothing open, waits, and passes
                // the exit status on (128 + n for death by signal n).
                for fd in 3..1024 {
                    libc::close(fd);
                }
                let mut status = 0;
                while libc::waitpid(pid, &mut status, 0) != pid {}
                libc::_exit(if libc::WIFEXITED(status) { libc::WEXITSTATUS(status) } else { 128 + libc::WTERMSIG(status) })
            }
        }
    }
}

struct Term {
    master: OwnedFd,
    /// Kept open so the terminal outlives the program and its modes can still be read.
    _slave: OwnedFd,
    child: Child,
    out: String,
    cols: usize,
    rows: usize,
}

impl Term {
    fn spawn(args: &[&str], cols: u16, rows: u16) -> Term {
        let (mut m, mut s) = (0, 0);
        let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: the out-pointers are valid for the call; the size is only read.
        let rc = unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &size as *const _ as *mut _) };
        assert_eq!(rc, 0, "openpty: {}", io::Error::last_os_error());
        // SAFETY: openpty returned two descriptors that nothing else owns.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        // SAFETY: fcntl on descriptors owned above.
        unsafe {
            libc::fcntl(m, libc::F_SETFL, libc::fcntl(m, libc::F_GETFL) | libc::O_NONBLOCK);
            libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(s, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let tty = || Stdio::from(slave.try_clone().expect("dup"));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pathwatch"));
        cmd.args(args).stdin(tty()).stdout(tty()).stderr(tty());
        cmd.env("TERM", "xterm-256color").env("COLORTERM", "truecolor");
        for name in ["NO_COLOR", "PATHWATCH_CONFIG", "PATHWATCH_NEXTDNS_PROFILE"] {
            cmd.env_remove(name);
        }
        // SAFETY: see `under_a_stand_in_shell`.
        unsafe { cmd.pre_exec(under_a_stand_in_shell) };
        let child = cmd.spawn().expect("spawn pathwatch");
        Term { master, _slave: slave, child, out: String::new(), cols: usize::from(cols), rows: usize::from(rows) }
    }

    /// Reads whatever the program has written, waiting up to `wait` for more.
    fn read(&mut self, wait: Duration) {
        let mut fds = [libc::pollfd { fd: self.master.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        // SAFETY: one valid pollfd; the buffer is valid for its length.
        unsafe {
            if libc::poll(fds.as_mut_ptr(), 1, wait.as_millis() as i32) <= 0 {
                return;
            }
            let mut buf = [0u8; 65536];
            loop {
                let n = libc::read(self.master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 {
                    break;
                }
                self.out.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
            }
        }
    }

    /// Waits until `done` holds, reading output meanwhile.
    #[track_caller]
    fn until(&mut self, what: &str, done: impl Fn(&mut Term) -> bool) {
        let start = Instant::now();
        loop {
            self.read(Duration::from_millis(20));
            if done(self) {
                return;
            }
            assert!(start.elapsed() < WAIT, "timed out waiting for: {what}\n--- screen ---\n{}", self.screen());
        }
    }

    /// Writes keys and returns at once, for timing what follows.
    fn keys_now(&mut self, keys: &str) {
        // SAFETY: the buffer is valid for its length.
        let n = unsafe { libc::write(self.master.as_raw_fd(), keys.as_ptr().cast(), keys.len()) };
        assert_eq!(n, keys.len() as isize, "write to the terminal");
    }

    fn keys(&mut self, keys: &str) {
        // One key at a time, as typed, so a lone Esc is not read as the
        // start of an escape sequence.
        for k in keys.chars() {
            let mut b = [0u8; 4];
            let bytes = k.encode_utf8(&mut b).as_bytes();
            // SAFETY: the buffer is valid for its length.
            let n = unsafe { libc::write(self.master.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(n, bytes.len() as isize, "write to the terminal");
            self.read(Duration::from_millis(40));
        }
    }

    /// What a terminal sends for the mouse, in the SGR encoding: button 0 is
    /// the left one, 64 and 65 the wheel away from and towards the user.
    /// Columns and rows count from 0 here, as on the screen. Written in one
    /// piece, as a terminal does.
    fn mouse(&mut self, button: u8, col: u16, row: u16) {
        let seq = format!("\x1b[<{button};{};{}M", col + 1, row + 1);
        // SAFETY: the buffer is valid for its length.
        let n = unsafe { libc::write(self.master.as_raw_fd(), seq.as_ptr().cast(), seq.len()) };
        assert_eq!(n, seq.len() as isize, "write to the terminal");
        self.read(Duration::from_millis(40));
    }

    /// Whether the terminal has been left reporting the mouse.
    fn reports_mouse(&self) -> bool {
        self.out.rfind(MOUSE_ON).is_some_and(|on| self.out.rfind(MOUSE_OFF).is_none_or(|off| off < on))
    }

    /// The process group in the terminal's foreground: the dashboard itself.
    fn foreground(&self) -> libc::pid_t {
        // SAFETY: tcgetpgrp on a descriptor this struct owns.
        let pid = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        assert!(pid > 0, "tcgetpgrp: {}", io::Error::last_os_error());
        pid
    }

    fn signal(&self, sig: libc::c_int) {
        // SAFETY: signalling a process this test started.
        assert_eq!(unsafe { libc::kill(self.foreground(), sig) }, 0, "kill: {}", io::Error::last_os_error());
    }

    /// Canonical, echoing input: what a shell expects to get back.
    fn cooked(&self) -> bool {
        // SAFETY: an all-zero termios is a valid value to be overwritten.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor and out-pointer.
        assert_eq!(unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut t) }, 0, "tcgetattr: {}", io::Error::last_os_error());
        t.c_lflag & libc::ICANON != 0 && t.c_lflag & libc::ECHO != 0
    }

    fn in_alternate_screen(&self) -> bool {
        self.out.rfind(ALT_ENTER).is_some_and(|enter| self.out.rfind(ALT_LEAVE).is_none_or(|leave| leave < enter))
    }

    fn cursor_hidden(&self) -> bool {
        self.out.rfind("\x1b[?25l").is_some_and(|hide| self.out.rfind("\x1b[?25h").is_none_or(|show| show < hide))
    }

    /// What is on the screen now: the output replayed through the few
    /// control sequences the dashboard uses (cursor position, erase, and the
    /// alternate screen). Colours and attributes are ignored.
    fn screen(&self) -> String {
        let mut grid = vec![vec![' '; self.cols]; self.rows];
        let (mut x, mut y) = (0usize, 0usize);
        let mut chars = self.out.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\x1b' if chars.peek() == Some(&'[') => {
                    chars.next();
                    let mut params = String::new();
                    let end = loop {
                        match chars.next() {
                            Some(p) if p.is_ascii_digit() || matches!(p, ';' | '?' | ':' | '>' | '=' | ' ') => params.push(p),
                            other => break other,
                        }
                    };
                    let mut nums = params.trim_start_matches('?').split(';').map(|n| n.parse::<usize>().unwrap_or(0));
                    match end {
                        Some('H') => {
                            y = nums.next().unwrap_or(1).max(1) - 1;
                            x = nums.next().unwrap_or(1).max(1) - 1;
                        }
                        Some('J') => grid.iter_mut().for_each(|r| r.fill(' ')),
                        Some('K') if y < self.rows => grid[y][x.min(self.cols)..].fill(' '),
                        Some('h' | 'l') if params == "?1049" => {
                            grid.iter_mut().for_each(|r| r.fill(' '));
                            (x, y) = (0, 0);
                        }
                        _ => {}
                    }
                }
                '\x1b' => {
                    chars.next();
                }
                '\r' => x = 0,
                '\n' => y = (y + 1).min(self.rows - 1),
                c if !c.is_control() => {
                    if x < self.cols && y < self.rows {
                        grid[y][x] = c;
                    }
                    x += 1;
                }
                _ => {}
            }
        }
        grid.iter().map(|r| r.iter().collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
    }

    fn started(&mut self) {
        // A whole first frame: the cursor is hidden at the end of it.
        self.until("the dashboard to draw", |t| t.in_alternate_screen() && !t.cooked() && t.cursor_hidden() && t.screen().contains("pathwatch"));
    }

    /// Waits for the program to end and checks the terminal is as it was found.
    #[track_caller]
    fn exits_with(mut self, code: i32) -> String {
        let start = Instant::now();
        let status = loop {
            self.read(Duration::from_millis(20));
            if let Some(status) = self.child.try_wait().expect("wait") {
                break status;
            }
            assert!(start.elapsed() < WAIT, "the program did not exit\n--- screen ---\n{}", self.screen());
        };
        self.read(Duration::from_millis(50));
        assert_eq!(status.code(), Some(code), "exit status\n--- output tail ---\n{}", self.out.chars().rev().take(600).collect::<String>().chars().rev().collect::<String>());
        assert!(!self.in_alternate_screen(), "the alternate screen was left");
        assert!(!self.cursor_hidden(), "the cursor is visible again");
        assert!(self.cooked(), "the terminal is back in canonical, echoing mode");
        assert!(!self.reports_mouse(), "the mouse is the terminal's again");
        self.out.clone()
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        // A failed assertion must not leave a dashboard running.
        // SAFETY: signalling processes this test started.
        unsafe {
            let fg = libc::tcgetpgrp(self.master.as_raw_fd());
            if fg > 0 {
                libc::kill(fg, libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        // Keep reading while it goes: on macOS a process cannot finish
        // exiting until the output it left in the terminal has been taken.
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) && !matches!(self.child.try_wait(), Ok(Some(_))) {
            self.read(Duration::from_millis(20));
        }
    }
}

#[test]
fn quitting_after_using_every_screen_restores_the_terminal() {
    let mut t = Term::spawn(&["demo", "isolated-failure", "--at", "90"], 120, 35);
    t.started();
    // Focus, mode, family and back, window, freeze, cursor on / move / off.
    // Each Esc is followed by a wait for what it does: two escapes that
    // arrive together are indistinguishable from the start of a key sequence.
    t.keys("2mMff+- chhH");
    t.until("the cursor readout", |t| t.screen().contains("cursor -") && t.screen().contains("FROZEN"));
    t.keys("\x1b");
    t.until("the cursor to go", |t| !t.screen().contains("cursor -"));
    // Details: every tab, scroll to the end and back, another provider.
    t.keys("\rl");
    t.until("the Distribution tab", |t| t.screen().contains("[Distribution]"));
    t.keys("ll");
    t.until("the Raw tab", |t| t.screen().contains("[Raw]") && t.screen().contains("sent, UTC"));
    t.keys("jjGkg4l");
    t.until("the Events tab for provider 4", |t| t.screen().contains("[Events]") && t.screen().contains("4 Arelion"));
    // Help over it, closed with Esc; Esc again goes back to the overview.
    t.keys("?");
    t.until("help", |t| t.screen().contains("quit and restore the terminal"));
    t.keys("\x1b");
    t.until("help to close", |t| !t.screen().contains("quit and restore the terminal") && t.screen().contains("[Events]"));
    t.keys("\x1b");
    t.until("the overview", |t| t.screen().contains("Outcomes") && !t.screen().contains("[Events]"));
    // Ctrl-L erases and redraws everything, without asking the terminal anything.
    let erases = t.out.matches("\x1b[2J").count();
    t.keys("\x0c");
    t.until("a full redraw", |t| t.out.matches("\x1b[2J").count() > erases && t.screen().contains("Outcomes") && t.screen().contains("pathwatch"));
    assert!(!t.out.contains("\x1b[6n"), "the dashboard never waits on a cursor-position reply");
    // A demo has no collector to stop, and says so instead of pretending.
    t.keys("P");
    t.until("the note", |t| t.screen().contains("no collection to stop"));
    t.keys("q");
    t.exits_with(0);
}

#[test]
fn ctrl_c_and_termination_signals_restore_the_terminal() {
    let mut t = Term::spawn(&["demo"], 100, 30);
    t.started();
    t.keys("\x03");
    t.exits_with(0);
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        let mut t = Term::spawn(&["demo"], 100, 30);
        t.started();
        t.signal(sig);
        t.exits_with(0);
    }
}

#[test]
fn a_panic_restores_the_terminal_before_its_message_is_printed() {
    let mut t = Term::spawn(&["demo", "--panic-after-ms", "600"], 100, 30);
    t.started();
    let out = t.exits_with(101);
    let (leave, message) = (out.rfind(ALT_LEAVE).expect("left the alternate screen"), out.rfind("forced panic").expect("the panic message is shown"));
    assert!(message > leave, "the message lands on the normal screen, where it stays readable");
}

#[test]
fn ctrl_z_hands_the_terminal_back_and_fg_takes_it_again() {
    let mut t = Term::spawn(&["demo"], 100, 30);
    t.started();
    let pid = t.foreground();
    t.keys("\x1a");
    t.until("the terminal to be handed back", |t| t.cooked() && !t.in_alternate_screen() && !t.cursor_hidden());
    let stopped = |pid: libc::pid_t| {
        let state = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().expect("ps");
        String::from_utf8_lossy(&state.stdout).contains('T')
    };
    let start = Instant::now();
    while !stopped(pid) {
        assert!(start.elapsed() < WAIT, "the process is stopped, as after Ctrl-Z in a shell");
        std::thread::sleep(Duration::from_millis(20));
    }
    let drawn = t.out.len();
    t.read(Duration::from_millis(300));
    assert_eq!(t.out.len(), drawn, "a stopped dashboard draws nothing");

    t.signal(libc::SIGCONT); // what `fg` does
    t.until("the dashboard to take the terminal again", |t| t.in_alternate_screen() && !t.cooked() && t.cursor_hidden() && t.screen().contains("no-reply"));
    t.keys("q");
    t.exits_with(0);
}

#[test]
fn small_terminals_get_a_request_and_no_terminal_gets_a_refusal() {
    let mut t = Term::spawn(&["demo"], 70, 20);
    t.until("the size request", |t| t.screen().contains("needs at least 80"));
    assert_eq!(t.screen().trim().lines().count(), 1, "nothing else is drawn: {}", t.screen());
    t.keys("q");
    t.exits_with(0);

    let out = Command::new(env!("CARGO_BIN_EXE_pathwatch")).arg("demo").stdin(Stdio::null()).output().expect("run");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty() && !out.stderr.contains(&0x1b), "no escape sequence is written into a pipe");
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs a terminal"));
}

#[test]
fn stopping_and_resuming_a_live_session_is_confirmed_stated_and_recorded() {
    // Loopback only: nothing leaves this machine. Where the host does not
    // permit unprivileged ICMP the stream is shown as unsupported, and the
    // stop, resume, and gap record are exercised all the same.
    let config = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("terminal-loopback.toml");
    std::fs::write(
        &config,
        "[general]\nfamilies = [\"ipv4\"]\n[icmp]\ninterval = \"200ms\"\n[[providers.cloudflare.targets]]\nid = \"lo4\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n\
         [[providers.google.targets]]\nid = \"lo4b\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n",
    )
    .expect("write the test configuration");
    let mut t = Term::spawn(&["run", "--config", config.to_str().expect("utf-8 path")], 120, 35);
    t.started();
    t.until("a live session", |t| t.screen().contains("COLLECTING"));

    t.keys("P");
    t.until("the question", |t| t.screen().contains("Stop collection?"));
    assert!(t.screen().contains("COLLECTING"), "asking is not stopping");
    t.keys("n");
    t.until("the question to go away", |t| !t.screen().contains("Stop collection?"));
    assert!(t.screen().contains("COLLECTING"));

    t.keys("Py");
    t.until("the stopped state", |t| t.screen().contains("STOPPED") && t.screen().contains("nothing is being sent"));
    std::thread::sleep(Duration::from_millis(1200));
    t.keys("P");
    t.until("collection to resume", |t| t.screen().contains("COLLECTING") && !t.screen().contains("STOPPED"));

    // The stop is in the record: both events and one gap of about its length.
    t.keys("\rh");
    t.until("the Events tab", |t| t.screen().contains("[Events]"));
    t.until("the gap and both events", |t| {
        let s = t.screen();
        s.contains("observation gap of") && s.contains("(stopped)") && s.contains("collection stopped by the user") && s.contains("collection resumed by the user")
    });
    t.keys("q");
    t.exits_with(0);
}

/// Whether this host lets an unprivileged program open an ICMP socket.
///
/// macOS always does. Linux decides by `net.ipv4.ping_group_range`, and where
/// that excludes the user — stock Ubuntu, GitHub's Ubuntu runners — a live
/// ICMP session has nothing to measure: the streams read `UNSUPPORTED`, which
/// other tests here check. A test of what a live ICMP session does says so
/// and stops. Where the host is known to permit it, `PATHWATCH_REQUIRE_ICMP`
/// turns that into a failure, so the tests cannot be skipped by accident.
#[track_caller]
fn live_icmp() -> bool {
    use pathwatch_core::model::Family;
    use pathwatch_core::platform::icmp_sock::IcmpSocket;
    match IcmpSocket::open(Family::V4) {
        Ok(_) => true,
        Err(e) => {
            let why = pathwatch_core::capability::explain_icmp_open_error(Family::V4, &e);
            assert!(std::env::var_os("PATHWATCH_REQUIRE_ICMP").is_none_or(|v| v.is_empty()), "PATHWATCH_REQUIRE_ICMP is set and {why}");
            eprintln!("not run at {}: needs a live ICMP session, and {why}", std::panic::Location::caller());
            false
        }
    }
}

// ───────────────────────── recording, export, replay ─────────────────────────

fn pathwatch(args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pathwatch"));
    cmd.args(args).stdin(Stdio::null());
    for name in ["PATHWATCH_CONFIG", "PATHWATCH_NEXTDNS_PROFILE", "PATHWATCH_TEST_PROFILE"] {
        cmd.env_remove(name);
    }
    cmd
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch directory");
    d
}

/// The one session directory a recording run left under `root`.
fn session_dir(root: &std::path::Path) -> std::path::PathBuf {
    let mut dirs: Vec<_> = std::fs::read_dir(root).expect("recording root").flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    assert_eq!(dirs.len(), 1, "one session: {dirs:?}");
    dirs.remove(0)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Loopback ICMP, and a "NextDNS" DoH target on a closed loopback port whose
/// URL carries a profile id: every send stays on this machine.
const RECORDED: &str = "[general]\nfamilies = [\"ipv4\"]\n[icmp]\ninterval = \"200ms\"\n[[providers.cloudflare.targets]]\nid = \"lo4\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n\
    [providers.google]\nenabled = false\n[providers.nextdns]\nenabled = true\nprofile_id_env = \"PATHWATCH_TEST_PROFILE\"\n\
    [[providers.nextdns.targets]]\nid = \"profile-doh\"\nurl = \"https://127.0.0.1:9/{profile_id}\"\nprotocols = [\"doh\"]\n";
const PROFILE: &str = "zz99profile";

#[test]
fn a_recorded_session_exports_in_every_format_and_never_contains_the_profile_id() {
    let dir = scratch("record-export");
    let (config, root) = (dir.join("config.toml"), dir.join("recordings"));
    std::fs::write(&config, RECORDED).unwrap();
    let run = pathwatch(&["sample", "--config", config.to_str().unwrap(), "--duration", "1500ms", "--format", "summary", "--record", root.to_str().unwrap()])
        .env("PATHWATCH_TEST_PROFILE", PROFILE)
        .output()
        .expect("run sample");
    assert_eq!(run.status.code(), Some(0), "{}", text(&run.stderr));
    assert!(text(&run.stderr).contains("recorded ") && text(&run.stderr).contains(root.to_str().unwrap()), "says where the recording went: {}", text(&run.stderr));
    assert!(!text(&run.stdout).contains(PROFILE) && !text(&run.stderr).contains(PROFILE));

    let session = session_dir(&root);
    let segment = session.join("000001.jsonl");
    let raw = std::fs::read_to_string(&segment).expect("first segment");
    assert!(raw.starts_with("{\"schema\":1,\"kind\":\"segment\""), "{}", &raw[..raw.len().min(120)]);
    assert!(raw.contains("\"redacted\":true") && raw.contains("‹redacted:nextdns-profile›"));
    assert!(!raw.contains(PROFILE), "the profile id is on disk");
    assert!(raw.contains("\"kind\":\"observation\"") && raw.contains("nextdns/profile-doh/doh/4"), "the stream that uses the profile did run");

    let export = |format: &str, path: &std::path::Path| {
        let out = pathwatch(&["export", path.to_str().unwrap(), "--format", format]).output().expect("run export");
        assert_eq!(out.status.code(), Some(0), "{format}: {}", text(&out.stderr));
        assert!(!text(&out.stdout).contains(PROFILE) && !text(&out.stderr).contains(PROFILE), "{format}: the profile id is in an export");
        (text(&out.stdout), text(&out.stderr))
    };
    let (jsonl, _) = export("jsonl", &session);
    assert_eq!(jsonl, raw, "a JSONL export of an intact recording is the recording");
    let (csv, _) = export("csv", &session);
    let rows: Vec<&str> = csv.lines().collect();
    assert!(rows[0].starts_with("stream,seq,attempt,sent_utc") && rows.len() > 1, "{csv}");
    assert!(rows[1..].iter().all(|r| r.split(',').count() >= 14), "every row has every column");
    assert_eq!(rows.len() - 1, raw.matches("\"kind\":\"observation\"").count(), "one row per observation");
    let (summary, warnings) = export("summary", &session);
    assert!(warnings.is_empty(), "{warnings}");
    for stream in ["cloudflare/lo4/icmp/4", "nextdns/profile-doh/doh/4"] {
        assert!(summary.lines().any(|l| l.starts_with(stream)), "{summary}");
    }

    // Cut the file mid-line: everything complete is still used, with a warning.
    let cut = dir.join("cut.jsonl");
    std::fs::write(&cut, &raw.as_bytes()[..raw.len() - 40]).unwrap();
    let (short, warnings) = export("jsonl", &cut);
    assert!(warnings.contains("incomplete") && warnings.contains("cut.jsonl"), "{warnings}");
    assert_eq!(short.lines().count(), raw.lines().count() - 1);
    assert!(raw.starts_with(&short));

    // Things that are not recordings are refused with a reason and a status.
    let missing = pathwatch(&["export", dir.join("nothing-here").to_str().unwrap()]).output().unwrap();
    assert_eq!(missing.status.code(), Some(5), "{}", text(&missing.stderr));
    let no_tty = pathwatch(&["replay", session.to_str().unwrap()]).output().unwrap();
    assert_eq!(no_tty.status.code(), Some(2));
    assert!(text(&no_tty.stderr).contains("needs a terminal") && !no_tty.stdout.contains(&0x1b));
}

#[test]
fn a_recording_that_cannot_be_started_is_an_error_before_anything_is_sent() {
    if !live_icmp() {
        return;
    }
    let dir = scratch("record-refused");
    let config = dir.join("config.toml");
    std::fs::write(&config, RECORDED).unwrap();
    // A file where the directory should be.
    let blocked = dir.join("not-a-directory");
    std::fs::write(&blocked, "x").unwrap();
    let run = pathwatch(&["sample", "--config", config.to_str().unwrap(), "--duration", "5s", "--record", blocked.to_str().unwrap()]).output().unwrap();
    assert_eq!(run.status.code(), Some(5), "{}", text(&run.stderr));
    assert!(text(&run.stderr).contains("cannot record to") && run.stdout.is_empty(), "no record was produced: {}", text(&run.stderr));
}

#[test]
fn a_live_session_records_and_its_recording_replays_as_a_replay() {
    let dir = scratch("record-replay");
    let (config, root) = (dir.join("config.toml"), dir.join("recordings"));
    std::fs::write(&config, RECORDED).unwrap();
    let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap(), "--record", root.to_str().unwrap()], 120, 35);
    t.started();
    t.until("the recorder to show in the header", |t| t.screen().contains("COLLECTING") && t.screen().contains("REC 0."));
    std::thread::sleep(Duration::from_millis(1500));
    t.keys("q");
    let out = t.exits_with(0);
    assert!(out.rfind("recorded ").is_some_and(|said| said > out.rfind(ALT_LEAVE).unwrap()), "where the recording went is said on the normal screen");

    let session = session_dir(&root);
    // The recording is whole: it ends with the session's closing record, and
    // has everything the collector reported before the dashboard was quit.
    let raw = std::fs::read_to_string(session.join("000001.jsonl")).unwrap();
    let last = raw.lines().last().unwrap();
    assert!(last.contains("\"kind\":\"session\"") && last.contains("\"state\":\"end\"") && last.contains("\"counters\""), "{last}");
    assert_eq!(raw.matches("\"kind\":\"session\"").count(), 2, "one opening and one closing record");

    let mut t = Term::spawn(&["replay", session.to_str().unwrap()], 120, 35);
    t.started();
    t.until("the replay", |t| t.screen().contains("REPLAY ENDED") && t.screen().contains("nothing is being sent"));
    let screen = t.screen();
    assert!(!screen.contains("COLLECTING") && !screen.contains("LIVE"), "a replay never calls itself live: {screen}");
    assert!(screen.contains("1 Cloudflare") && screen.contains("3 NextDNS") && screen.contains("127.0.0.1"), "the recorded providers and target: {screen}");
    // A replay has no collector: it cannot be stopped, and it says so.
    t.keys("P");
    t.until("the note", |t| t.screen().contains("no collection to stop"));
    // Details work on recorded data as on live data.
    t.keys("\rlll");
    t.until("the Raw tab", |t| t.screen().contains("[Raw]") && t.screen().contains("sent, UTC"));
    t.keys("q");
    t.exits_with(0);

    // Played from the start at speed, it is a replay in progress.
    let mut t = Term::spawn(&["replay", session.to_str().unwrap(), "--speed", "0.5"], 120, 35);
    t.started();
    t.until("a replay in progress", |t| t.screen().contains("REPLAY") && !t.screen().contains("REPLAY ENDED") && t.screen().contains("0.5× speed"));
    t.keys("q");
    t.exits_with(0);
}

/// A scripted session as a recording file whose session id, provider name,
/// stream ids and event text all carry `hostile`.
fn hostile_recording(dir: &std::path::Path, hostile: &str) -> std::path::PathBuf {
    use pathwatch_core::model::{EventRec, Line, Msg, Record, SessionRec, StreamRec};
    let sc = pathwatch_core::script::by_name("isolated-failure").expect("a built-in scenario");
    let streams = sc.streams.iter().map(|s| StreamRec {
        stream: s.info.id.clone(),
        provider: s.info.provider.clone(),
        target: s.info.target.clone(),
        protocol: s.info.protocol,
        family: s.info.family,
        configured: s.configured.into(),
        interval_ms: s.info.interval.as_millis() as u64,
        deadline_ms: s.info.deadline.as_millis() as u64,
        state: s.state.into(),
        reason: None,
    });
    let start = SessionRec {
        session: "s-MARK".into(),
        state: "start".into(),
        utc: "2026-11-21T00:00:00.000Z".into(),
        app: "pathwatch".into(),
        version: "test".into(),
        os: "test".into(),
        arch: "test".into(),
        streams: streams.collect(),
        counters: None,
        traffic: Vec::new(),
    };
    let note = EventRec { session: "s-MARK".into(), at_ns: 1, utc: "2026-11-21T00:00:00.000Z".into(), event: "note".into(), stream: None, detail: "a note MARK".into() };
    let mut lines = vec![Line::new(Record::Session(start)).to_json(), Line::new(Record::Event(note)).to_json()];
    lines.extend(pathwatch_core::script::generate_timed(&sc, 7).into_iter().filter_map(|(_, m)| if let Msg::Record(r) = m { Some(Line::new(r).to_json()) } else { None }));
    // In the file as JSON carries it: `\u001b` for the controls, the rest raw.
    let escaped = serde_json::to_string(hostile).expect("a string");
    let escaped = escaped.trim_matches('"');
    let scripted = format!("\"session\":\"{}\"", sc.session());
    let text = (lines.join("\n") + "\n").replace(&scripted, "\"session\":\"s-MARK\"").replace("MARK", escaped).replace("resolver-v4", &format!("resolver-v4{escaped}"));
    let path = dir.join("hostile.jsonl");
    std::fs::write(&path, text).expect("write the recording");
    path
}

#[test]
fn a_recording_from_someone_else_cannot_write_to_the_terminal() {
    // Set the window title, write the clipboard, a C1 control, an override
    // of the reading direction: what a recording could carry if its text
    // reached the terminal as it is in the file.
    const HOSTILE: &str = "\u{1b}]0;INJECTED\u{7}\u{1b}]52;c;eA==\u{1b}\\\u{9b}31m\u{202e}";
    let dir = scratch("hostile");
    let recording = hostile_recording(&dir, HOSTILE);
    let raw = std::fs::read_to_string(&recording).unwrap();
    assert!(raw.contains("\\u001b]0;INJECTED") && raw.contains('\u{202e}'), "the file carries it");
    let unsafe_in = |s: &str| s.chars().filter(|c| *c != '\n').any(pathwatch_core::sanitize::is_unsafe);

    for format in ["jsonl", "csv", "summary"] {
        let out = pathwatch(&["export", recording.to_str().unwrap(), "--format", format]).output().expect("run export");
        assert_eq!(out.status.code(), Some(0), "{format}: {}", text(&out.stderr));
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert!(!unsafe_in(&stdout) && !unsafe_in(&stderr), "{format}: a control character or an override was written:\n{stdout:?}\n{stderr:?}");
        // Nothing is hidden either: it is there, as text.
        assert!(stdout.contains("INJECTED"), "{format}: {stdout}");
        if format != "jsonl" {
            assert!(stdout.contains("\\x1b]0;INJECTED\\x07"), "{format}: shown as text: {stdout}");
        }
    }

    let mut t = Term::spawn(&["replay", recording.to_str().unwrap()], 120, 35);
    t.started();
    t.until("the replay", |t| t.screen().contains("REPLAY ENDED"));
    // Every tab of the provider whose stream ids carry it, and the events.
    t.keys("\r");
    for _ in 0..5 {
        t.keys("l");
    }
    t.until("the Events tab", |t| t.screen().contains("[Events]"));
    t.keys("q");
    let out = t.exits_with(0);
    assert!(out.contains("INJECTED"), "the text was drawn");
    for sequence in ["\u{1b}]0;", "\u{1b}]52;", "\u{7}", "\u{9b}", "\u{202e}"] {
        assert!(!out.contains(sequence), "{sequence:?} reached the terminal");
    }
}

#[test]
fn controls_run_as_a_line_of_their_own_on_a_live_session() {
    if !live_icmp() {
        return;
    }
    // Both controls given literal loopback addresses: nothing is discovered
    // and nothing leaves this machine. Port 53 on loopback is normally
    // closed, which is a fine thing for a control to report.
    let dir = scratch("controls");
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        "[general]\nfamilies = [\"ipv4\"]\n[icmp]\ninterval = \"200ms\"\n[[providers.cloudflare.targets]]\nid = \"lo4\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n\
         [providers.google]\nenabled = false\n[controls.gateway]\nenabled = true\naddress = \"127.0.0.2\"\n[controls.local_dns]\nenabled = true\naddress = \"127.0.0.3\"\n",
    )
    .unwrap();
    let root = dir.join("recordings");
    let check = pathwatch(&["config", "check", "--config", config.to_str().unwrap()]).output().unwrap();
    assert_eq!(check.status.code(), Some(0), "controls are valid configuration: {}", text(&check.stderr));
    let listed = text(&check.stdout);
    assert!(listed.contains("gateway/configured/icmp/4") && listed.contains("local-dns/configured/dns-udp/4"), "the check lists what a session would run: {listed}");
    assert!(listed.contains("steady-state rate: 10.20"), "five echoes a second to each loopback target, and one query every five seconds: {listed}");

    let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap(), "--record", root.to_str().unwrap()], 120, 35);
    t.started();
    t.until("the controls line", |t| t.screen().lines().any(|l| l.contains("Gateway 127.0.0.2") && l.contains("Local DNS (stub) 127.0.0.3")));
    let screen = t.screen();
    assert_eq!(screen.lines().nth(2).map(|l| l.matches("╭─ ").count()), Some(4), "four cards, as always: {screen}");
    assert!(!screen.contains("╭─ 5") && screen.contains("1 Cloudflare"));
    // The header says where probes are being sent from, by address and interface.
    t.until("the source context in the header", |t| t.screen().lines().next().is_some_and(|l| l.contains("via 127.0.0.1 on lo")));
    // And the context at start is the session's first event.
    t.keys("\rh");
    t.until("the source-context event", |t| t.screen().contains("[Events]") && t.screen().contains("source context 1: IPv4 127.0.0.1 on lo"));
    t.keys("\x1b");
    std::thread::sleep(Duration::from_millis(600));
    t.keys("q");
    t.exits_with(0);

    // What the controls reveal about this network — the gateway's address and
    // the resolver in use — is in the recording, and leaves with --redact-local.
    let session = session_dir(&root);
    let export = |extra: &[&str]| {
        let out = pathwatch(&[&["export", session.to_str().unwrap(), "--format", "jsonl"], extra].concat()).output().expect("run export");
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        text(&out.stdout)
    };
    let plain = export(&[]);
    assert!(plain.contains("127.0.0.2") && plain.contains("127.0.0.3"), "recorded as measured");
    let redacted = export(&["--redact-local"]);
    assert!(!redacted.contains("127.0.0.2") && !redacted.contains("127.0.0.3"), "the control addresses are gone");
    // So is what the source context says in words: the interface's name and
    // the gateway, which is recorded whether or not a control probes it.
    assert!(plain.contains("source context 1: IPv4 127.0.0.1 on lo") && plain.contains("\"if\":\"lo"), "recorded as found");
    assert!(
        redacted.contains("source context 1: ‹redacted:local›") && !redacted.contains(" on lo") && !redacted.contains("gateway ") && !redacted.contains("\"if\":"),
        "{redacted}"
    );
    // On loopback the host's own address is also the provider target's, so
    // that goes too; the streams and every record remain.
    assert!(redacted.contains("‹redacted:local›") && redacted.contains("gateway/configured/icmp/4") && redacted.contains("cloudflare/lo4/icmp/4"), "the streams remain");
    assert_eq!(redacted.lines().count(), plain.lines().count(), "no record is dropped");
}

#[test]
fn doctor_reports_the_gateway_and_resolvers_it_would_use_without_sending_anything() {
    let out = pathwatch(&["doctor"]).output().expect("run doctor");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let report = text(&out.stdout);
    for line in ["source IPv4 ", "source IPv6 ", "gateway IPv4 ", "gateway IPv6 ", "system DNS "] {
        assert!(report.lines().any(|l| l.starts_with(line)), "{line} in:\n{report}");
    }
}

// ───────────────────────── configuration reload ─────────────────────────

/// One loopback stream, and a second provider switched off.
const BEFORE_RELOAD: &str = "[general]\nfamilies = [\"ipv4\"]\n[icmp]\ninterval = \"200ms\"\n[[providers.cloudflare.targets]]\nid = \"lo4\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n[providers.google]\nenabled = false\n";
/// The second provider switched on, with a loopback target of its own.
const AFTER_RELOAD: &str = "[general]\nfamilies = [\"ipv4\"]\n[icmp]\ninterval = \"200ms\"\n[[providers.cloudflare.targets]]\nid = \"lo4\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n[[providers.google.targets]]\nid = \"lo4\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n";

#[test]
fn the_r_key_reloads_a_valid_file_and_refuses_anything_else_without_changing_a_thing() {
    if !live_icmp() {
        return;
    }
    let dir = scratch("reload");
    let config = dir.join("config.toml");
    std::fs::write(&config, BEFORE_RELOAD).unwrap();
    let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap()], 120, 35);
    t.started();
    t.until("the second provider shown as switched off", |t| t.screen().contains("COLLECTING") && t.screen().contains("DISABLED"));
    // The Events tab is where a reload reports.
    t.keys("\rh");
    t.until("the Events tab", |t| t.screen().contains("[Events]"));

    // A file that does not parse as configuration: refused, nothing changed.
    std::fs::write(&config, format!("{BEFORE_RELOAD}[icmp]\nintervall = \"1s\"\n")).unwrap();
    t.keys("R");
    t.until("the refusal", |t| t.screen().contains("configuration not reloaded, and nothing was changed"));
    assert!(!t.screen().contains("configuration revision"), "no revision came of it");

    // A valid file that changes something only a restart can apply: refused whole.
    std::fs::write(&config, format!("{AFTER_RELOAD}[limits]\nburst = 3\n")).unwrap();
    t.keys("R");
    t.until("the restart-only refusal", |t| t.screen().contains("limits.burst") && t.screen().contains("restart to apply it"));
    assert!(!t.screen().contains("configuration revision"), "and the provider it also enabled was not started");

    // A valid file: a new revision, and the provider that was off is running.
    std::fs::write(&config, AFTER_RELOAD).unwrap();
    t.keys("R");
    t.until("revision 2", |t| t.screen().contains("configuration revision 2: 1 started, 0 stopped, 0 restarted, 1 unchanged"));
    t.keys("\x1b");
    t.until("both providers collecting", |t| {
        let s = t.screen();
        s.contains("Outcomes") && !s.contains("DISABLED") && s.lines().filter(|l| l.contains("127.0.0.1")).count() >= 1 && s.matches("ICMP · v4").count() == 4
    });
    let cards = t.screen().lines().skip(2).take(4).collect::<Vec<_>>().join("\n");
    assert!(cards.matches("127.0.0.1").count() == 2, "two cards with a target now:\n{cards}");

    // A hang-up with the terminal still there is a reload too, not an exit.
    t.signal(libc::SIGHUP);
    t.keys("\r");
    t.until("revision 3 from the signal", |t| t.screen().contains("configuration revision 3: 0 started, 0 stopped, 0 restarted, 2 unchanged"));
    t.keys("q");
    t.exits_with(0);
}

#[test]
fn a_hang_up_from_a_closed_terminal_still_ends_a_live_session() {
    let dir = scratch("hangup");
    let (config, root) = (dir.join("config.toml"), dir.join("recordings"));
    std::fs::write(&config, BEFORE_RELOAD).unwrap();
    let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap(), "--record", root.to_str().unwrap()], 120, 35);
    t.started();
    t.until("a live session", |t| t.screen().contains("COLLECTING") && t.screen().contains("REC 0."));
    let dashboard = t.foreground();
    // Close the terminal: put something else in the master's place, so the
    // last reference to the pseudo-terminal goes away and the kernel hangs
    // up on the session.
    let null = std::fs::File::open("/dev/null").expect("/dev/null");
    // SAFETY: both descriptors are open and owned by this test.
    assert!(unsafe { libc::dup2(null.as_raw_fd(), t.master.as_raw_fd()) } >= 0);
    // The stand-in shell dies of the hang-up, as a shell would. The
    // dashboard is not this test's child, so its end is seen by it being gone.
    let start = Instant::now();
    while still_running(dashboard) {
        assert!(start.elapsed() < WAIT, "the session outlived its terminal");
        std::thread::sleep(Duration::from_millis(20));
    }
    // And it ended in order, not by being killed and not by trying to
    // reload: the recording is closed with the session's last record.
    let raw = std::fs::read_to_string(session_dir(&root).join("000001.jsonl")).unwrap();
    let last = raw.lines().last().unwrap();
    assert!(last.contains("\"kind\":\"session\"") && last.contains("\"state\":\"end\""), "{last}");
    assert!(!raw.contains("\"kind\":\"config\"") && !raw.contains("config_reload_failed"), "a closed terminal is not a request to reload");
}

#[test]
fn headless_collection_reloads_on_hang_up_and_says_what_happened() {
    if !live_icmp() {
        return;
    }
    let dir = scratch("reload-headless");
    let config = dir.join("config.toml");
    std::fs::write(&config, BEFORE_RELOAD).unwrap();
    let child =
        pathwatch(&["sample", "--config", config.to_str().unwrap(), "--duration", "4s", "--quiet"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("run sample");
    let hang_up = |child: &Child| {
        // SAFETY: signalling a process this test started.
        assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGHUP) }, 0);
    };
    std::thread::sleep(Duration::from_millis(1_000));
    // First a file that parses but does not validate, then a good one.
    std::fs::write(&config, format!("{BEFORE_RELOAD}[controls.gateway]\naddress = \"the router\"\n")).unwrap();
    hang_up(&child);
    std::thread::sleep(Duration::from_millis(700));
    std::fs::write(&config, AFTER_RELOAD).unwrap();
    hang_up(&child);
    let out = child.wait_with_output().expect("sample finishes");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));

    let said = text(&out.stderr);
    let refused = said.find("configuration not reloaded, and nothing was changed").expect("the bad file is refused");
    let applied = said.find("configuration reloaded").expect("the good file is applied");
    assert!(refused < applied && said.contains("is invalid"), "{said}");

    let records = text(&out.stdout);
    let revisions: Vec<&str> = records.lines().filter(|l| l.contains("\"kind\":\"config\"")).collect();
    assert_eq!(revisions.len(), 1, "one revision: the refused file left no trace in the records");
    assert!(revisions[0].contains("\"rev\":2") && revisions[0].contains("1 started, 0 stopped, 0 restarted, 1 unchanged"), "{}", revisions[0]);
    // The stream that ran throughout is tagged with the revision in force; the new one only ever with the second.
    let of = |stream: &str, rev: u32| {
        records
            .lines()
            .filter(|l| l.contains("\"kind\":\"observation\"") && l.contains(&format!("\"stream\":\"{stream}\"")) && l.contains(&format!("\"config_rev\":{rev},")))
            .count()
    };
    assert!(of("cloudflare/lo4/icmp/4", 1) >= 5 && of("cloudflare/lo4/icmp/4", 2) >= 5, "{} then {}", of("cloudflare/lo4/icmp/4", 1), of("cloudflare/lo4/icmp/4", 2));
    assert!(of("google/lo4/icmp/4", 1) == 0 && of("google/lo4/icmp/4", 2) >= 5);
}

/// A frozen view is redrawn once a second: it has only a clock to show.
/// A key must not wait for that second — what it asks for comes from a
/// new snapshot, made at once — and neither must leaving.
#[test]
fn a_frozen_view_ticks_slowly_and_still_answers_at_once() {
    let mut t = Term::spawn(&["demo", "dns-only"], 120, 35);
    t.started();
    t.until("the demo", |t| t.screen().contains("ICMP · v4"));
    t.keys(" ");
    t.until("the frozen view", |t| t.screen().contains("FROZEN"));
    // Past the tick that showed it, so the next one is most of a second away.
    t.read(Duration::from_millis(150));
    let asked = Instant::now();
    t.keys_now("m");
    t.until("the cards in the new mode", |t| t.screen().contains("DNS/UDP · v4"));
    let answered = asked.elapsed();
    assert!(answered < Duration::from_millis(600), "the mode key waited for the slow tick: {answered:?}");
    // The count of what has arrived since still moves — a second at a
    // time, not ten times a second.
    t.read(Duration::from_millis(300));
    let (watching, mut seen, mut last) = (Instant::now(), 0, t.screen());
    while watching.elapsed() < Duration::from_millis(2_600) {
        t.read(Duration::from_millis(20));
        let now = t.screen();
        if now != last {
            (seen, last) = (seen + 1, now);
        }
    }
    assert!((1..=6).contains(&seen), "a frozen screen changed {seen} times in 2.6 s");
    // Unfreezing is a change of view too.
    t.read(Duration::from_millis(150));
    let asked = Instant::now();
    t.keys_now(" ");
    t.until("the live view", |t| !t.screen().contains("FROZEN"));
    assert!(asked.elapsed() < Duration::from_millis(600), "unfreezing waited for the slow tick: {:?}", asked.elapsed());
    t.keys(" ");
    t.until("frozen again", |t| t.screen().contains("FROZEN"));
    t.read(Duration::from_millis(150));
    let asked = Instant::now();
    t.keys("q");
    t.exits_with(0);
    assert!(asked.elapsed() < Duration::from_millis(700), "leaving waited for the slow tick: {:?}", asked.elapsed());
}

// ───────────────────────── sample --assert ─────────────────────────

/// One DNS stream to a loopback port where nothing listens: every query
/// is refused at once, on any host, and nothing leaves this machine.
const REFUSED: &str = "[general]\nfamilies = [\"ipv4\"]\n[icmp]\nenabled = false\n[dns.doh]\nenabled = false\n[dns]\ninterval = \"300ms\"\ndeadline = \"200ms\"\n\
    [[providers.cloudflare.targets]]\nid = \"lo\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\", \"dns-udp\"]\n[providers.google]\nenabled = false\n";

fn sample_with(config: &std::path::Path, extra: &[&str]) -> (Option<i32>, String, String) {
    let mut args = vec!["sample", "--config", config.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = pathwatch(&args).output().expect("run sample");
    (out.status.code(), text(&out.stdout), text(&out.stderr))
}

#[test]
fn an_assertion_turns_what_was_measured_into_the_exit_status() {
    let dir = scratch("assert");
    let config = dir.join("config.toml");
    std::fs::write(&config, REFUSED).unwrap();
    let stream = "cloudflare/lo/dns-udp/4";

    // True of what was measured: success, and each check is listed.
    let (code, _, said) = sample_with(&config, &["--duration", "2s", "--quiet", "--assert", "sent >= 3", "--assert", "replied == 0", "--assert", "*/dns-udp/4: no-reply >= 3"]);
    assert_eq!(code, Some(0), "{said}");
    assert_eq!(said.lines().filter(|l| l.starts_with("assert ok ") && l.contains(stream)).count(), 0, "passing checks are not listed when quiet: {said}");
    let (code, _, said) = sample_with(&config, &["--count", "4", "--format", "summary", "--assert", "replied == 0"]);
    assert_eq!(code, Some(0), "{said}");
    assert!(said.contains(&format!("assert ok       {stream}: replied 0 of ")), "{said}");

    // False: exit 10, saying what was measured and what was asserted. The summary is still written.
    let (code, summary, said) = sample_with(&config, &["--duration", "2s", "--format", "summary", "--assert", "no-reply == 0", "--assert", "sent >= 1"]);
    assert_eq!(code, Some(10), "{said}");
    assert!(said.contains(&format!("assert FAILED   {stream}: no-reply ")) && said.contains(", asserted == 0"), "{said}");
    assert!(said.contains("1 of 2 assertion checks did not hold") && said.contains("(exit 10)"), "{said}");
    assert!(summary.contains(stream) && summary.contains("no-reply"), "{summary}");

    // Not enough to judge is not a pass: no round-trip time exists, and a
    // percentage is not taken from a handful of probes.
    let (code, _, said) = sample_with(&config, &["--duration", "2s", "--quiet", "--assert", "p50 < 500ms"]);
    assert_eq!(code, Some(10), "{said}");
    assert!(said.contains(&format!("assert NO DATA  {stream}: p50: 0 round-trip times in the window; p50 needs 5")), "{said}");
    let (code, _, said) = sample_with(&config, &["--duration", "2s", "--quiet", "--assert", "no-reply <= 100%"]);
    assert_eq!(code, Some(10), "{said}");
    assert!(said.contains("a percentage needs 20"), "{said}");
}

#[test]
fn an_assertion_that_could_never_be_checked_is_refused_before_anything_is_sent() {
    let dir = scratch("assert-usage");
    let config = dir.join("config.toml");
    std::fs::write(&config, REFUSED).unwrap();
    for (extra, says) in [
        (vec!["--duration", "1s", "--assert", "loss < 1%"], "unknown metric \"loss\""),
        (vec!["--duration", "1s", "--assert", "p50 < 40"], "give the value in ms"),
        (vec!["--assert", "sent >= 1"], "--assert needs --duration or --count"),
        (vec!["--duration", "1s", "--assert", "google: sent >= 1"], "selects no stream. The streams are:"),
        // The ICMP stream is in the file and switched off.
        (vec!["--duration", "1s", "--assert", "*/icmp/4: sent >= 1"], "selects only streams that will not run in this sample"),
    ] {
        let (code, records, said) = sample_with(&config, &extra);
        assert_eq!(code, Some(2), "{extra:?}: {said}");
        assert!(said.contains(says), "{extra:?}: {said}");
        // Not so much as the record that opens a session: collection never began.
        assert!(records.is_empty(), "refused with nothing sent: {extra:?}: {records}");
    }
}

/// The summary as sentences: for a screen reader, or to be read a line at a
/// time. Nothing outside ASCII, no table, and the same facts.
#[test]
fn the_plain_format_says_the_summary_in_sentences() {
    let dir = scratch("plain");
    let config = dir.join("config.toml");
    std::fs::write(&config, REFUSED).unwrap();
    let (code, said, _) = sample_with(&config, &["--duration", "2s", "--format", "plain"]);
    assert_eq!(code, Some(0));
    assert!(said.is_ascii(), "{said}");
    assert!(said.starts_with("pathwatch sample: 2 seconds, statistics over a window of 5m. Times are in milliseconds.\n"), "{said}");
    assert!(said.contains("\nStream cloudflare/lo/dns-udp/4, to 127.0.0.1.\n"), "{said}");
    let probes = said.lines().find(|l| l.starts_with("  Probes: ")).expect("the counts");
    assert!(probes.contains(" sent, 0 answered, 0 timed out, ") && probes.contains(" unreachable, 0 error responses, "), "{probes}");
    assert!(said.contains("  Round-trip time: none, because no probe was answered as expected.\n"), "{said}");
    assert!(said.contains(" times, connection refused"), "the commonest failure, in words: {said}");
    // What was not measured is said, and said not to be loss; what is switched off is left out.
    assert!(said.contains("Provider 3 NextDNS: needs configuration: ") && said.contains("Nothing was sent; this is not counted as loss."), "{said}");
    assert!(!said.contains("icmp/4"), "the ICMP stream is switched off in this file: {said}");
    // No table: no line is a row of columns padded with runs of spaces.
    assert!(said.lines().all(|l| !l.trim_start().contains("   ")), "{said}");
    assert!(said.trim_end().ends_with("`pathwatch glossary` explains every term."));
}

#[test]
fn the_glossary_command_explains_every_status_word_as_text_or_markdown() {
    let out = pathwatch(&["glossary"]).output().expect("run glossary");
    assert_eq!(out.status.code(), Some(0));
    let said = text(&out.stdout);
    for word in ["OK", "WARMING UP", "NO REPLY", "LOSSY", "SLOW", "ERRORS", "LOCAL ERR", "STALE", "NEEDS CONFIG", "UNSUPPORTED", "DISABLED", "no-reply", "p95", "cov"] {
        assert!(said.contains(&format!("\n{word}\n  ")), "{word} has no entry:\n{said}");
    }
    assert!(said.contains("It is not proof of packet loss on your line"), "{said}");
    let md = text(&pathwatch(&["glossary", "--markdown"]).output().expect("run glossary").stdout);
    assert!(md.starts_with("# Glossary\n") && md.contains("\n## Status words\n") && md.contains("| `NO REPLY` | 5 consecutive probes went unanswered"), "{md}");
}

/// The accessibility options on the dashboard itself: bright colours on an
/// explicit black ground, frames for new data held to four a second, and
/// keys answered as promptly as ever.
#[test]
fn high_contrast_and_reduced_motion_are_options_of_the_dashboard() {
    let mut t = Term::spawn(&["demo", "--theme", "high-contrast", "--reduced-motion", "--color", "always"], 120, 35);
    t.started();
    t.until("the demo", |t| t.screen().contains("DEMO") && t.screen().contains("1 Cloudflare"));
    // White on black is asked for outright, not left to the terminal's palette.
    let codes: std::collections::BTreeSet<&str> =
        t.out.split("\x1b[").filter_map(|rest| rest.split_once('m').map(|(code, _)| code)).filter(|c| c.chars().all(|ch| ch.is_ascii_digit() || ch == ';')).collect();
    assert!(codes.contains("38;5;15;48;5;0"), "no explicit white on black in the output: {codes:?}");
    // Every colour set comes with its ground, and nothing is dimmed.
    assert!(codes.iter().filter(|c| c.starts_with("38;")).all(|c| c.contains(";48;5;")), "a colour was left on the terminal's own ground: {codes:?}");
    assert!(!codes.contains("2"), "something was dimmed: {codes:?}");
    // The screen still moves with the data, and a key does not wait for the next frame.
    let before = t.screen();
    t.until("the playing demo to move on", |t| t.screen() != before);
    let pressed = Instant::now();
    t.keys("2");
    t.until("the focus to move", |t| t.screen().lines().last().is_some_and(|l| l.starts_with(" 2 Google")));
    assert!(pressed.elapsed() < Duration::from_millis(1_500), "a key was held back: {:?}", pressed.elapsed());
    t.keys("q");
    t.exits_with(0);
}

/// A check whose sample was interrupted has no result, and does not report one.
#[test]
fn an_interrupted_sample_does_not_claim_its_assertions_held() {
    let dir = scratch("assert-interrupted");
    let config = dir.join("config.toml");
    std::fs::write(&config, REFUSED).unwrap();
    let records = dir.join("records.jsonl");
    let child = pathwatch(&["sample", "--config", config.to_str().unwrap(), "--duration", "30s", "--quiet", "--output", records.to_str().unwrap(), "--assert", "sent >= 0"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("run sample");
    // Interrupt it once it is collecting, however long it took to start.
    let started = Instant::now();
    while std::fs::metadata(&records).map_or(0, |m| m.len()) == 0 {
        assert!(started.elapsed() < WAIT, "the sample never began");
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: signalling a process this test started.
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) }, 0);
    let out = child.wait_with_output().expect("sample finishes");
    assert_eq!(out.status.code(), Some(130));
    let said = text(&out.stderr);
    assert!(said.contains("the assertions were not checked") && !said.contains("assert ok"), "{said}");
}

// ───────────────────────── route tracing ─────────────────────────

#[test]
fn a_route_trace_is_proposed_then_run_on_its_own_socket_and_stops_by_itself() {
    if !live_icmp() {
        return;
    }
    // Traced address: this machine's loopback. The destination answers at
    // the first hop, so the trace is two rounds of one probe and nothing
    // leaves the host. Reverse lookups are off: they would ask a resolver.
    let dir = scratch("trace");
    let config = dir.join("config.toml");
    std::fs::write(&config, format!("{BEFORE_RELOAD}[trace]\nrounds = 2\nround_interval = \"300ms\"\nreverse_dns = false\n")).unwrap();
    let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap()], 120, 35);
    t.started();
    t.until("a live session", |t| t.screen().contains("COLLECTING") && t.screen().contains("127.0.0.1"));

    // `t` shows what would be sent, and sends nothing.
    t.keys("t");
    t.until("the proposal", |t| t.screen().contains("Route to 127.0.0.1") && t.screen().contains("not started"));
    let proposal = t.screen();
    assert!(proposal.contains("Nothing has been sent.") && proposal.contains("2 rounds") && proposal.contains("at most 300 probes"), "{proposal}");

    // Enter runs it; it finds the destination at hop 1 and finishes by itself.
    t.keys("\r");
    t.until("the finished trace", |t| t.screen().contains("finished") && t.screen().contains("127.0.0.1 — destination"));
    let done = t.screen();
    assert!(done.contains("round 2 of 2") && done.contains("probes 2 of 300"), "two probes, as proposed: {done}");
    let hop = done.lines().find(|l| l.contains("127.0.0.1 — destination")).unwrap();
    assert!(hop.trim_start_matches(['│', ' ']).starts_with("1 ") && hop.contains("2/2"), "{hop}");
    // The measurement carried on beside it, untouched.
    t.keys("t");
    t.until("the note", |t| t.screen().contains("no trace is running"));
    t.keys("\x1b");
    t.until("the overview", |t| t.screen().contains("Outcomes") && t.screen().contains("● OK"));
    assert!(t.screen().contains("no-reply 0.0 %") || t.screen().contains("no-reply 0 /"), "{}", t.screen());
    t.keys("q");
    t.exits_with(0);
}

#[test]
fn route_tracing_is_not_offered_where_nothing_can_be_sent() {
    let mut t = Term::spawn(&["demo"], 120, 35);
    t.started();
    t.keys("t");
    t.until("the explanation", |t| t.screen().contains("route tracing needs a live session"));
    assert!(!t.screen().contains("Route to"));
    t.keys("q");
    t.exits_with(0);
}

// ───────────────────────── the mouse ─────────────────────────

#[test]
fn the_mouse_is_left_alone_unless_asked_for() {
    let mut t = Term::spawn(&["demo"], 120, 35);
    t.started();
    t.until("the first provider focused", |t| t.screen().lines().last().is_some_and(|l| l.starts_with(" 1 Cloudflare")));
    // What a click on the second card would send, had the terminal been asked to report it.
    t.mouse(0, 45, 5);
    t.keys("+");
    t.until("the key after it to have been handled", |t| t.screen().lines().last().is_some_and(|l| l.contains("15 m")));
    assert!(t.screen().lines().last().unwrap().starts_with(" 1 Cloudflare"), "the click did nothing:\n{}", t.screen());
    t.keys("q");
    let out = t.exits_with(0);
    assert!(!out.contains(MOUSE_ON) && !out.contains(MOUSE_OFF), "the terminal was never asked about the mouse");
}

#[test]
fn with_the_mouse_a_click_focuses_and_places_the_cursor_and_the_wheel_changes_the_window() {
    let mut t = Term::spawn(&["demo", "--mouse"], 120, 35);
    t.started();
    let footer = |t: &Term| t.screen().lines().last().unwrap_or("").to_string();
    t.until("the first provider focused", |t| footer(t).starts_with(" 1 Cloudflare") && footer(t).contains(" 5 m "));
    assert!(t.reports_mouse(), "asked for, so the terminal is told to report it");
    assert!(!t.out.contains("\x1b[?1003h") && !t.out.contains("\x1b[?1002h"), "presses and the wheel only: no reports of the pointer moving");

    // The second card.
    t.mouse(0, 45, 5);
    t.until("the second provider focused", |t| footer(t).starts_with(" 2 Google"));
    // The wheel over the chart: towards the user is a longer window, away a shorter one.
    t.mouse(65, 60, 15);
    t.until("a longer window", |t| footer(t).contains(" 15 m "));
    t.mouse(64, 60, 15);
    t.mouse(64, 60, 15);
    t.until("a shorter window", |t| footer(t).contains(" 1 m "));
    // A click on the chart puts the cursor there; Esc takes it away, as ever.
    t.mouse(0, 60, 15);
    t.until("the cursor's read-out", |t| footer(t).contains("Esc off"));
    t.keys("\x1b");
    t.until("the cursor gone", |t| footer(t).starts_with(" 2 Google"));
    // The help says what the mouse does, and a click under it does nothing.
    t.keys("?");
    t.until("the help panel", |t| t.screen().contains("a card, table row or legend name: focus that provider"));
    t.mouse(0, 5, 5);
    // `?` again turns to what the words and numbers mean, which scrolls; once more closes it.
    t.keys("?");
    t.until("the second page of help", |t| t.screen().contains("What the words and numbers mean") && t.screen().contains("j k scroll"));
    assert!(!t.screen().contains("outcome strip"), "the end of the page is below the fold:\n{}", t.screen());
    t.keys("G");
    t.until("the end of that page", |t| t.screen().contains("outcome strip"));
    t.keys("?");
    t.until("the help closed", |t| !t.screen().contains("What the words and numbers mean") && footer(t).starts_with(" 2 Google"));
    // Details: a tab, then the wheel on its list.
    t.keys("\r");
    t.until("details, with the whole tab bar drawn", |t| t.screen().lines().any(|l| l.contains("[Streams]") && l.contains("Raw") && l.contains("Events")));
    // Wherever the tab bar is, the word "Raw" on it is the Raw tab.
    let screen = t.screen();
    let (row, bar) = screen.lines().enumerate().find(|(_, l)| l.contains("[Streams]")).expect("the tab bar");
    let col = bar.find("Raw").map(|at| bar[..at].chars().count()).expect("the Raw tab on the tab bar");
    t.mouse(0, col as u16 + 1, row as u16);
    t.until("the Raw tab", |t| t.screen().contains("[Raw]") && t.screen().contains("rows 1–"));
    t.mouse(65, 60, 15);
    t.until("the list three rows on", |t| t.screen().contains("rows 4–"));
    t.keys("q");
    t.exits_with(0);
}

#[test]
fn the_mouse_is_handed_back_on_every_way_out() {
    // Suspended: the shell must not be sent clicks. Resumed: it is ours again.
    let mut t = Term::spawn(&["demo", "--mouse"], 100, 30);
    t.started();
    assert!(t.reports_mouse());
    t.keys("\x1a");
    t.until("the terminal to be handed back", |t| t.cooked() && !t.in_alternate_screen() && !t.reports_mouse());
    t.signal(libc::SIGCONT);
    t.until("the dashboard to take the terminal again", |t| t.in_alternate_screen() && !t.cooked() && t.reports_mouse() && t.screen().contains("no-reply"));
    t.keys("\x03");
    t.exits_with(0);

    // A termination signal, and a hang-up.
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        let mut t = Term::spawn(&["demo", "--mouse"], 100, 30);
        t.started();
        assert!(t.reports_mouse());
        t.signal(sig);
        t.exits_with(0);
    }

    // A panic: handed back before the message, which lands on a terminal that is whole.
    let mut t = Term::spawn(&["demo", "--mouse", "--panic-after-ms", "600"], 100, 30);
    t.started();
    assert!(t.reports_mouse());
    let out = t.exits_with(101);
    assert!(out.rfind("forced panic").unwrap() > out.rfind(MOUSE_OFF).unwrap(), "the mouse first, then the message");
}

#[test]
fn the_configuration_file_can_ask_for_the_mouse_on_a_live_session() {
    let dir = scratch("mouse");
    let config = dir.join("config.toml");
    std::fs::write(&config, format!("{BEFORE_RELOAD}[ui]\nmouse = true\n")).unwrap();
    let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap()], 120, 35);
    t.started();
    t.until("a live session", |t| t.screen().contains("COLLECTING"));
    assert!(t.reports_mouse(), "[ui] mouse = true");
    // The second card is a provider switched off; it can be focused all the same, as with `2`.
    t.mouse(0, 45, 5);
    t.until("the second provider focused", |t| t.screen().lines().last().is_some_and(|l| l.starts_with(" 2 Google")));
    t.keys("q");
    t.exits_with(0);
}

// ───────────────────────── performance (run on request) ─────────────────────────

/// Measurements against the budgets in `docs/design/verification.md`
/// §5 that need no network. Ignored in an ordinary test run: they take
/// minutes, want a quiet machine, and mean something only in a release
/// build. Every destination is this machine's own loopback.
///
/// ```sh
/// cargo test --release -p pathwatch --test terminal perf -- --ignored --nocapture --test-threads=1
/// ```
///
/// `PATHWATCH_PERF_SECONDS` sets how long each steady measurement runs
/// (default 120). Each test prints `PERF |` rows and fails if a budget it
/// measures is exceeded.
mod perf {
    use super::*;

    /// The plan's workload W1 in shape and rate — four providers, both
    /// families, ICMP, DNS over UDP and DoH, both controls — with every
    /// destination on loopback. DNS and DoH are refused at once, so what
    /// they cost in a TLS handshake and an answer is not in these numbers.
    fn workload() -> String {
        let mut text = String::from("[controls.gateway]\nenabled = true\naddress = \"127.0.0.1\"\n[controls.local_dns]\nenabled = true\naddress = \"127.0.0.1\"\n");
        for provider in ["cloudflare", "google", "nextdns", "arelion"] {
            text.push_str(&format!("[providers.{provider}]\nenabled = true\n"));
            for (id, key, value, protocols) in [
                ("lo4", "address", "127.0.0.1", "\"icmp\", \"dns-udp\""),
                ("lo6", "address", "::1", "\"icmp\", \"dns-udp\""),
                ("doh4", "url", "https://127.0.0.1:9/dns-query", "\"doh\""),
                ("doh6", "url", "https://[::1]:9/dns-query", "\"doh\""),
            ] {
                text.push_str(&format!("[[providers.{provider}.targets]]\nid = \"{id}\"\n{key} = \"{value}\"\nprotocols = [{protocols}]\n"));
            }
        }
        text
    }

    fn config(name: &str) -> std::path::PathBuf {
        let path = scratch(name).join("config.toml");
        std::fs::write(&path, workload()).unwrap();
        path
    }

    fn seconds() -> u64 {
        std::env::var("PATHWATCH_PERF_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(120)
    }

    fn release_build() {
        if cfg!(debug_assertions) {
            panic!("these are measurements of a release build: add --release");
        }
    }

    /// (median, 95th percentile by nearest rank, maximum)
    fn spread(mut values: Vec<f64>) -> (f64, f64, f64) {
        values.sort_by(|a, b| a.total_cmp(b));
        let at = |p: f64| values[(((p * values.len() as f64).ceil() as usize).max(1) - 1).min(values.len() - 1)];
        (at(0.5), at(0.95), values[values.len() - 1])
    }

    /// Prints one result and returns whether it is within its budget.
    fn row(what: &str, budget: &str, measured: String, within: bool) -> bool {
        println!("PERF | {what} | {budget} | {measured} | {}", if within { "within" } else { "OVER" });
        within
    }

    /// CPU time the process has used, in seconds.
    fn cpu_seconds(pid: libc::pid_t) -> f64 {
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // utime and stime: the 12th and 13th fields after the name in brackets, in clock ticks.
            let fields: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
            let ticks: f64 = fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap();
            // SAFETY: sysconf has no preconditions.
            return ticks / unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        }
        // Elsewhere, `ps` prints it as [hours:]minutes:seconds.hundredths.
        let out = Command::new("ps").args(["-o", "cputime=", "-p", &pid.to_string()]).output().expect("ps");
        text(&out.stdout).trim().split(':').fold(0.0, |total, part| total * 60.0 + part.parse::<f64>().unwrap_or(0.0))
    }

    /// Resident memory, in megabytes.
    fn resident_mb(pid: libc::pid_t) -> f64 {
        let out = Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().expect("ps");
        text(&out.stdout).trim().parse::<f64>().unwrap_or(0.0) / 1024.0
    }

    /// Reads the terminal for `time`, discarding what was read so a long
    /// measurement does not grow, and returns (bytes written, CPU seconds
    /// used, largest resident memory seen).
    fn watch(t: &mut Term, pid: libc::pid_t, time: Duration) -> (usize, f64, f64) {
        t.out.clear();
        let (start, cpu) = (Instant::now(), cpu_seconds(pid));
        let (mut bytes, mut peak, mut sampled) = (0, resident_mb(pid), Instant::now());
        while start.elapsed() < time {
            t.read(Duration::from_millis(50));
            bytes += t.out.len();
            t.out.clear();
            if sampled.elapsed() >= Duration::from_secs(10) {
                peak = peak.max(resident_mb(pid));
                sampled = Instant::now();
            }
        }
        (bytes, cpu_seconds(pid) - cpu, peak.max(resident_mb(pid)))
    }

    #[test]
    #[ignore = "a measurement; see the module comment"]
    fn perf_environment() {
        release_build();
        let uname = Command::new("uname").arg("-srm").output().map(|o| text(&o.stdout).trim().to_string()).unwrap_or_default();
        println!("PERF | host | — | {uname}, {} cores | —", std::thread::available_parallelism().map_or(0, |n| n.get()));
        println!("PERF | terminal | — | a pseudo-terminal read by the test: no emulator's drawing time is in any number | —");
        println!("PERF | workload | — | 34 streams on loopback, 12.4 transactions a second; DNS and DoH are refused at once | —");
    }

    #[test]
    #[ignore = "a measurement; see the module comment"]
    fn perf_start_up_and_shutdown() {
        release_build();
        let config = config("perf-start");
        let mut ok = true;
        for (name, args) in [("demo", vec!["demo"]), ("live, on loopback", vec!["run", "--config", config.to_str().unwrap()])] {
            let (mut first_frame, mut restored, mut exited) = (Vec::new(), Vec::new(), Vec::new());
            for _ in 0..15 {
                let begun = Instant::now();
                let mut t = Term::spawn(&args, 120, 35);
                t.started();
                first_frame.push(begun.elapsed().as_secs_f64() * 1000.0);
                // Let it settle into its ordinary state before asking it to leave.
                t.read(Duration::from_millis(300));
                t.out.clear();
                let asked = Instant::now();
                t.keys_now("q");
                // The alternate screen is left in the same write that restores the terminal.
                while !t.out.contains("\x1b[?1049l") {
                    assert!(asked.elapsed() < WAIT, "the terminal was not restored");
                    t.read(Duration::from_millis(5));
                }
                restored.push(asked.elapsed().as_secs_f64() * 1000.0);
                while t.child.try_wait().expect("wait").is_none() {
                    assert!(asked.elapsed() < WAIT, "the program did not exit");
                    t.read(Duration::from_millis(5));
                }
                exited.push(asked.elapsed().as_secs_f64() * 1000.0);
            }
            let (m, p, x) = spread(first_frame);
            ok &= row(&format!("start-up, {name}: first whole frame"), "≤ 300 ms", format!("median {m:.0}, p95 {p:.0}, max {x:.0} ms of 15"), x <= 300.0);
            let (m, p, x) = spread(restored);
            ok &= row(&format!("shutdown, {name}: terminal restored"), "≤ 100 ms", format!("median {m:.0}, p95 {p:.0}, max {x:.0} ms of 15"), x <= 100.0);
            let (m, p, x) = spread(exited);
            ok &= row(&format!("shutdown, {name}: process gone"), "≤ 3 s", format!("median {m:.0}, p95 {p:.0}, max {x:.0} ms of 15"), x <= 3000.0);
        }
        assert!(ok, "a budget was exceeded; see the PERF rows");
    }

    #[test]
    #[ignore = "a measurement; see the module comment"]
    fn perf_key_to_screen() {
        release_build();
        let config = config("perf-keys");
        let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap()], 120, 35);
        t.started();
        t.until("a live session", |t| t.screen().contains("COLLECTING"));
        // The focus keys: each redraws the footer with the provider's name.
        let names = ["Cloudflare", "Google", "NextDNS", "Arelion"];
        let mut times = Vec::new();
        for i in 0..200usize {
            // Not in step with the ten-a-second redraw.
            std::thread::sleep(Duration::from_millis(37 + (i as u64 * 13) % 61));
            t.read(Duration::ZERO);
            t.out.clear();
            let focus = (i + 1) % 4;
            let pressed = Instant::now();
            t.keys_now(&(focus + 1).to_string());
            while !t.out.contains(names[focus]) {
                assert!(pressed.elapsed() < WAIT, "key {} changed nothing on screen", focus + 1);
                t.read(Duration::from_millis(5));
            }
            times.push(pressed.elapsed().as_secs_f64() * 1000.0);
        }
        let (m, p, x) = spread(times);
        let ok = row("key written to the terminal → changed output read back", "p95 < 100 ms", format!("median {m:.1}, p95 {p:.1}, max {x:.1} ms of 200"), p < 100.0);
        // The application's own share is inside that figure, so a figure under its budget settles both.
        let inner = row("of which the application: key read → frame flushed", "p95 ≤ 50 ms", format!("not timed separately; bounded by the p95 above, {p:.1} ms"), p <= 50.0);
        t.keys("q");
        t.exits_with(0);
        assert!(ok && inner, "a budget was exceeded; see the PERF rows");
    }

    #[test]
    #[ignore = "a measurement; see the module comment"]
    fn perf_steady_then_frozen() {
        release_build();
        let config = config("perf-steady");
        let time = Duration::from_secs(seconds());
        // `PATHWATCH_PERF_ARGS` adds options, to see what one of them costs or saves.
        let extra = std::env::var("PATHWATCH_PERF_ARGS").unwrap_or_default();
        let mut args = vec!["run", "--config", config.to_str().unwrap()];
        args.extend(extra.split_whitespace());
        let mut t = Term::spawn(&args, 120, 35);
        t.started();
        t.until("a live session", |t| t.screen().contains("COLLECTING"));
        let pid = t.foreground();
        // The dashboard as it opens: live view, five-minute window.
        std::thread::sleep(Duration::from_secs(10));
        let early = resident_mb(pid);
        let (bytes, cpu, peak) = watch(&mut t, pid, time);
        let secs = time.as_secs_f64();
        // Three per cent since 2026-11-22: restated from two, which this
        // workload sat on. See `docs/design/verification.md` §5.
        let mut ok = row("CPU, live view", "mean ≤ 3 % of one core", format!("{:.2} % over {secs:.0} s", cpu / secs * 100.0), cpu / secs <= 0.03);
        ok &= row("terminal output, live view", "mean ≤ 20 kB/s", format!("{:.1} kB/s over {secs:.0} s", bytes as f64 / secs / 1000.0), bytes as f64 / secs <= 20_000.0);
        // Frozen: collection goes on, the screen has nothing new to show.
        t.keys_now(" ");
        std::thread::sleep(Duration::from_secs(3));
        let (bytes, cpu, peak_frozen) = watch(&mut t, pid, time);
        ok &= row("CPU, frozen and idle", "mean ≤ 1 % of one core", format!("{:.2} % over {secs:.0} s", cpu / secs * 100.0), cpu / secs <= 0.01);
        ok &= row("terminal output, frozen and idle", "≤ 2 kB/s", format!("{:.2} kB/s over {secs:.0} s", bytes as f64 / secs / 1000.0), bytes as f64 / secs <= 2_000.0);
        let (peak, last) = (peak.max(peak_frozen), resident_mb(pid));
        ok &= row(
            "resident memory",
            "≤ 64 MB at any time",
            format!("{early:.1} MB after 10 s, {last:.1} MB after {:.0} s, largest seen {peak:.1} MB", 13.0 + 2.0 * secs),
            peak <= 64.0,
        );
        t.out.clear();
        t.keys("q");
        t.exits_with(0);
        assert!(ok, "a budget was exceeded; see the PERF rows");
    }

    /// Where the live dashboard's CPU time goes, by the system's sampling
    /// profiler. macOS only; prints the busiest functions.
    #[test]
    #[ignore = "a measurement; see the module comment"]
    fn perf_profile_of_the_live_dashboard() {
        release_build();
        if !cfg!(target_os = "macos") {
            println!("PERF | profile | — | only where the `sample` tool exists | —");
            return;
        }
        let config = config("perf-profile");
        let report = config.with_file_name("profile.txt");
        let mut t = Term::spawn(&["run", "--config", config.to_str().unwrap()], 120, 35);
        t.started();
        t.until("a live session", |t| t.screen().contains("COLLECTING"));
        let pid = t.foreground();
        let mut profiler =
            Command::new("sample").args([&pid.to_string(), "20", "-file", report.to_str().unwrap()]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("run sample");
        while profiler.try_wait().expect("wait").is_none() {
            t.read(Duration::from_millis(50));
            t.out.clear();
        }
        t.keys("q");
        t.exits_with(0);
        let text = std::fs::read_to_string(&report).expect("the profile");
        println!("PERF | profile written to | — | {} | —", report.display());
        for line in text.lines().skip_while(|l| !l.starts_with("Sort by top of stack")).take(40) {
            println!("{line}");
        }
    }

    #[test]
    #[ignore = "a measurement; see the module comment"]
    fn perf_scheduling_and_traffic() {
        release_build();
        let config = config("perf-sched");
        let records = config.with_file_name("records.jsonl");
        let time = seconds();
        let out =
            pathwatch(&["sample", "--config", config.to_str().unwrap(), "--duration", &format!("{time}s"), "--output", records.to_str().unwrap()]).output().expect("run sample");
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let summary = text(&out.stderr);
        // How late each probe left, against the slot it was scheduled for.
        let mut lags: Vec<f64> = std::fs::read_to_string(&records)
            .unwrap()
            .lines()
            .filter(|l| l.contains("\"kind\":\"observation\""))
            .filter_map(|l| l.split("\"sched_lag_us\":").nth(1)?.split([',', '}']).next()?.parse::<f64>().ok())
            .map(|us| us / 1000.0)
            .collect();
        assert!(lags.len() > 100, "{} probes with a scheduling lag", lags.len());
        let n = lags.len();
        lags.sort_by(|a, b| a.total_cmp(b));
        let p99 = lags[((0.99 * n as f64).ceil() as usize).max(1) - 1];
        let worst = lags[n - 1];
        let mut ok = row("probe scheduling lag", "p99 ≤ 5 ms, max ≤ 50 ms", format!("p99 {p99:.2}, max {worst:.2} ms of {n} probes in {time} s"), p99 <= 5.0 && worst <= 50.0);
        // The receive loop's lateness, checked twenty times a second: the
        // 99th percentile is within 2 ms when no more than one check in a
        // hundred found it later than that.
        let worst_lag: f64 = summary.split("worst receive-loop lag ").nth(1).and_then(|s| s.split(' ').next()).and_then(|s| s.parse().ok()).expect("the worst lag in the summary");
        let counts: Vec<f64> = summary.split("over 2 ms in ").nth(1).expect("the checks in the summary").split(' ').filter_map(|w| w.parse().ok()).take(2).collect();
        let (over, checks) = (counts[0], counts[1]);
        // How long replies waited for this process after the kernel had
        // stamped them: the receive loop's lateness where it matters.
        let mut waits: Vec<f64> = std::fs::read_to_string(&records)
            .unwrap()
            .lines()
            .filter(|l| l.contains("\"kind\":\"observation\""))
            .filter_map(|l| l.split("\"recv_delay_us\":").nth(1)?.split([',', '}']).next()?.parse::<f64>().ok())
            .map(|us| us / 1000.0)
            .collect();
        assert!(waits.len() > 100, "{} replies with a receive delay", waits.len());
        waits.sort_by(|a, b| a.total_cmp(b));
        let wait_p99 = waits[((0.99 * waits.len() as f64).ceil() as usize).max(1) - 1];
        ok &= row(
            "receive loop: replies waiting to be read",
            "p99 ≤ 2 ms",
            format!("p99 {wait_p99:.2}, max {:.2} ms of {} replies in {time} s", waits[waits.len() - 1], waits.len()),
            wait_p99 <= 2.0,
        );
        // The plan's own method, a timer checked twenty times a second. It
        // measures how late the system wakes a sleeping thread as much as
        // anything this program does, so it is reported and not judged.
        row(
            "receive loop: a 50 ms timer more than 2 ms late",
            "reported, not judged",
            format!("{over:.0} of {checks:.0} checks ({:.1} %); worst {worst_lag:.1} ms in {time} s", over / checks.max(1.0) * 100.0),
            true,
        );
        // Traffic against what the configuration implies, where that can be computed.
        let mut worst_diff: f64 = 0.0;
        let mut compared = 0;
        for line in summary.lines().filter(|l| l.starts_with("  icmp ") || l.starts_with("  dns-udp ")) {
            if let Some(diff) = line.split_whitespace().find(|w| w.ends_with('%') && w.starts_with(['+', '-'])).and_then(|w| w.trim_end_matches('%').parse::<f64>().ok()) {
                worst_diff = if diff.abs() > worst_diff.abs() { diff } else { worst_diff };
                compared += 1;
            }
        }
        assert!(compared >= 3, "{compared} traffic rows compared:\n{summary}");
        ok &= row("traffic sent against the configured budget", "within ±10 %", format!("furthest of {compared} kinds {worst_diff:+.1} % over {time} s"), worst_diff.abs() <= 10.0);
        assert!(ok, "a budget was exceeded; see the PERF rows\n{summary}");
    }
}
