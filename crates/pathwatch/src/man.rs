//! The manual page, made from the command definitions.
//!
//! Every command and option in it is read from the same structures that
//! parse the command line, so the page cannot list an option that does not
//! exist or miss one that does. The sections that are prose — what an exit
//! status means, which files and variables are read — are written here.
//! `docs/pathwatch.1` is this output, and a test keeps the two the same.

use clap::Command;

/// Text as roff: a backslash, a hyphen that must stay a hyphen, and a line
/// that would begin with a control character are all escaped.
fn esc(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if !out.is_empty() {
            out.push('\n');
        }
        let line = line.trim();
        if line.starts_with(['.', '\'']) {
            out.push_str("\\&");
        }
        for ch in line.chars() {
            match ch {
                '\\' => out.push_str("\\e"),
                '-' => out.push_str("\\-"),
                other => out.push(other),
            }
        }
    }
    out
}

fn options(cmd: &Command, out: &mut String) {
    for arg in cmd.get_arguments().filter(|a| !a.is_hide_set() && !matches!(a.get_id().as_str(), "help" | "version")) {
        let values: Vec<String> = arg.get_value_names().map(|names| names.iter().map(|n| n.to_string()).collect()).unwrap_or_default();
        let takes_value = arg.get_action().takes_values();
        let name = match (arg.get_long(), arg.is_positional()) {
            (Some(long), _) => {
                let value = if takes_value { format!(" \\fI{}\\fR", esc(&values.join(" "))) } else { String::new() };
                format!("\\fB\\-\\-{}\\fR{value}", esc(long))
            }
            (None, _) => format!("\\fI{}\\fR", esc(&values.join(" "))),
        };
        // Sentences, each ending in a full stop, whatever the option's own text did.
        let mut parts: Vec<String> =
            arg.get_help().map(|h| h.to_string().replace('\n', " ").trim().trim_end_matches('.').to_string()).filter(|h| !h.is_empty()).into_iter().collect();
        let possible: Vec<String> = arg.get_possible_values().iter().filter(|v| !v.is_hide_set()).map(|v| v.get_name().to_string()).collect();
        if takes_value && !possible.is_empty() {
            parts.push(format!("One of: {}", possible.join(", ")));
        }
        let defaults: Vec<String> = arg.get_default_values().iter().map(|v| v.to_string_lossy().into_owned()).collect();
        if takes_value && !defaults.is_empty() {
            parts.push(format!("Default: {}", defaults.join(" ")));
        }
        let help = if parts.is_empty() { String::new() } else { format!("{}.", parts.join(". ")) };
        out.push_str(&format!(".TP\n{name}\n{}\n", esc(&help)));
    }
}

fn commands(cmd: &Command, prefix: &str, out: &mut String) {
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set() && s.get_name() != "help") {
        let name = format!("{prefix} {}", sub.get_name());
        if sub.get_subcommands().next().is_some() {
            commands(sub, &name, out);
            continue;
        }
        out.push_str(&format!(".SS {}\n{}\n", esc(&name), esc(&sub.get_about().map(|a| a.to_string()).unwrap_or_default())));
        options(sub, out);
    }
}

const DESCRIPTION: &str = "\
pathwatch measures up to four DNS and network providers continuously \\(em by default Cloudflare and Google, with NextDNS and Arelion once their endpoints are configured \\(em and shows, with stated definitions, which is misbehaving, on which protocol and address family, and since when.
.PP
It sends ICMP echo requests and DNS queries over UDP, TCP, TLS and HTTPS to the addresses named in its configuration. A target given as a hostname is looked up through the system resolver, and so are the names of route hops in a trace you start; nothing else is sent anywhere. It runs without privilege and never changes a system setting. Where the host does not permit a kind of probe, that stream is reported as unsupported; it is never reported as the target failing.
.PP
With no command, \\fBpathwatch\\fR is \\fBpathwatch run\\fR.";

const EXIT_STATUS: &[(&str, &str)] = &[
    ("0", "The program ran and ended normally. This says nothing about the network, unless \\fB\\-\\-assert\\fR was given."),
    ("1", "The dashboard lost its terminal: it could not be written to or read from."),
    ("2", "A usage error: an option that cannot be understood, an assertion that could never be checked, or an interactive command without a terminal."),
    ("3", "The configuration is invalid. Every problem is listed."),
    ("4", "No stream could be started: every configured probe is unsupported or refused on this host."),
    (
        "5",
        "An error on a file that was asked for: a recording that cannot be written or read, an export, or records written to a pipe that closed before an asserted sample was complete.",
    ),
    ("10", "A \\fBsample \\-\\-assert\\fR check was false, or could not be evaluated."),
    ("101", "An unexpected internal error. The terminal is restored first."),
    ("130, 143", "Ended by SIGINT or SIGTERM in a headless command. The dashboard exits 0 however it is left."),
];

const ENVIRONMENT: &[(&str, &str)] = &[
    ("PATHWATCH_CONFIG", "The configuration file, when \\fB\\-\\-config\\fR is not given."),
    ("PATHWATCH_NEXTDNS_PROFILE", "Your NextDNS profile id. It is masked on screen and removed from recordings as they are written."),
    ("NO_COLOR", "When not empty, no colour is used. Bold and reverse video are kept."),
    ("COLORTERM, TERM", "How many colours the terminal has."),
    ("XDG_CONFIG_HOME, HOME", "Where the default configuration file and the recording directory are looked for."),
    ("HTTPS_PROXY, ALL_PROXY", "Noticed and reported. DNS over HTTPS connects to the resolver itself and does not use a proxy."),
];

/// The whole page.
pub fn page(root: &Command, version: &str) -> String {
    let mut out =
        format!(".\\\" Generated by `pathwatch man`. Do not edit: change the program and regenerate.\n.TH PATHWATCH 1 \"\" \"pathwatch {}\" \"User Commands\"\n", esc(version));
    out.push_str(".SH NAME\npathwatch \\- continuously measure four DNS and network providers\n");
    out.push_str(".SH SYNOPSIS\n.B pathwatch\n.RI [ command ]\n.RI [ options ]\n");
    out.push_str(&format!(".SH DESCRIPTION\n{DESCRIPTION}\n"));
    out.push_str(".SH COMMANDS\n");
    commands(root, "pathwatch", &mut out);
    out.push_str(
        ".SH KEYS\nIn the dashboard, \\fB?\\fR lists every key, and \\fB?\\fR again explains every word and number on screen. \\fBq\\fR quits and restores the terminal.\n",
    );
    out.push_str(".SH EXIT STATUS\n");
    for (code, meaning) in EXIT_STATUS {
        out.push_str(&format!(".TP\n.B {code}\n{meaning}\n"));
    }
    out.push_str(".SH ENVIRONMENT\n");
    for (name, meaning) in ENVIRONMENT {
        out.push_str(&format!(".TP\n.B {name}\n{meaning}\n"));
    }
    out.push_str(".SH FILES\n.TP\n.I ~/.config/pathwatch/config.toml\nThe configuration file, if it exists. \\fBpathwatch config path\\fR says which file is in use, \\fBpathwatch config example\\fR prints a commented one, and \\fBpathwatch config reference\\fR explains every setting.\n");
    out.push_str(".TP\n.I ~/.local/state/pathwatch/recordings/\nRecordings, one directory per session, when recording is asked for.\n");
    out.push_str(".SH NOTES\nOn Linux, ICMP needs \\fInet.ipv4.ping_group_range\\fR to include one of your groups. Where it does not, ICMP streams are reported as unsupported with the remedy, and everything else runs. \\fBpathwatch doctor\\fR reports what the host permits, and sends nothing.\n");
    out.push_str(
        ".PP\n\\(lqno\\-reply\\(rq is the share of probes that got no answer. It is not proof of packet loss on your line. \\fBpathwatch glossary\\fR explains every term.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_for_roff() {
        assert_eq!(esc("--record [DIR]"), "\\-\\-record [DIR]");
        assert_eq!(esc(".config and 'quoted'"), "\\&.config and 'quoted'");
        assert_eq!(esc("a\\b"), "a\\eb");
        assert_eq!(esc("  two\n  lines "), "two\nlines");
    }
}
