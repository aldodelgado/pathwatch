//! Every word and number the program shows, with what it means and what it
//! does not.
//!
//! One table feeds the dashboard's help, `pathwatch glossary`, and the
//! documentation, so the three cannot drift apart. The numbers in it are
//! taken from the constants the statistics and the incident rules use, and
//! the tests below hold the status words against the ones the dashboard
//! prints.

use crate::incident::IncidentConfig;
use crate::stats::{LOW_N_BELOW, MIN_N_CENTRAL, MIN_N_P95, MIN_N_P99, MIN_N_RATE, MIN_N_SD, MIN_PAIRS_DELTA};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Times,
    Shares,
    Counts,
    Status,
    Marks,
}

impl Group {
    pub const ALL: [Group; 5] = [Group::Times, Group::Shares, Group::Counts, Group::Status, Group::Marks];

    pub fn title(self) -> &'static str {
        match self {
            Group::Times => "Times",
            Group::Shares => "Shares",
            Group::Counts => "Counts",
            Group::Status => "Status words",
            Group::Marks => "Marks",
        }
    }

    /// One sentence that holds for every term in the group.
    pub fn preface(self) -> &'static str {
        match self {
            Group::Times => {
                "In milliseconds, from answered probes sent within the window, to the current destination only. An ICMP round trip ends at the kernel's receive stamp; DNS, DoT and DoH end at this program's read. A fresh or dns-tcp stream includes setting up its connection."
            }
            Group::Shares => "Of the probes sent within the window. Shown as a count (\"2 / 12\") until there are enough probes for a percentage to mean something.",
            Group::Counts => "Of the probe slots within the window. Every slot ends as exactly one of these.",
            Group::Status => {
                "One per stream, and one per card for its main stream. Carried by a symbol and a word, never by colour alone. The thresholds are the defaults; `[incidents]` in the configuration changes them."
            }
            Group::Marks => "What stands in a cell instead of, or beside, a number, and what the outcome strip under a chart is made of.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// As it appears on screen.
    pub label: &'static str,
    pub group: Group,
    /// One line, for the help panel.
    pub short: String,
    /// The whole of it, for the glossary and the documentation.
    pub detail: String,
}

/// The longest `label` and `short` the help panel has room for at 80 columns.
pub const LABEL_WIDTH: usize = 15;
pub const SHORT_WIDTH: usize = 58;

fn term(label: &'static str, group: Group, short: impl Into<String>, detail: impl Into<String>) -> Term {
    Term { label, group, short: short.into(), detail: detail.into() }
}

/// Every term, in the order they are explained.
pub fn terms() -> Vec<Term> {
    use Group::*;
    let d = IncidentConfig::default();
    let secs = |ns: u64| ns / 1_000_000_000;
    let (central, low, p95, p99, sd, pairs, rate) = (MIN_N_CENTRAL, LOW_N_BELOW, MIN_N_P95, MIN_N_P99, MIN_N_SD, MIN_PAIRS_DELTA, MIN_N_RATE);
    vec![
        term(
            "last",
            Times,
            "the newest answered round trip",
            "The round-trip time of the most recent probe that was answered as expected. It is the newest answer in the session, not in the window, so it can be older than the window; a stream that has stopped answering keeps showing its last one.",
        ),
        term(
            "p50",
            Times,
            format!("the median round trip; needs {central} answers"),
            format!(
                "The median of the answered round trips, by nearest rank: the value halfway up the sorted times, rounding the rank up, so always a time that was actually measured. Withheld below {central} answers, and marked as resting on few below {low}."
            ),
        ),
        term(
            "p95",
            Times,
            format!("the 95th percentile, nearest rank; needs {p95} answers"),
            format!("The time that 95 % of answered round trips did not exceed, by nearest rank. Withheld below {p95} answers: with fewer, it would silently be the maximum."),
        ),
        term("p99", Times, format!("the 99th percentile; needs {p99} answers"), format!("As p95, for 99 %. Withheld below {p99} answers.")),
        term(
            "mean",
            Times,
            format!("the arithmetic mean; needs {central}; pulled by outliers"),
            format!(
                "The arithmetic mean of the answered round trips. One slow answer moves it, so read it beside the median. Withheld below {central} answers, marked below {low}."
            ),
        ),
        term(
            "min  max",
            Times,
            "the fastest and the slowest answered round trip",
            "The extremes of the answered round trips. The maximum is the number most sensitive to this host being busy at the moment a reply arrived.",
        ),
        term(
            "sd",
            Times,
            format!("sample standard deviation; needs {sd} answers"),
            format!(
                "The sample standard deviation, dividing by n - 1, of the answered round trips. It assumes nothing about their shape, and latency is usually skewed. Withheld below {sd} answers."
            ),
        ),
        term(
            "Δ",
            Times,
            format!("mean change between consecutive answers; needs {pairs} pairs"),
            format!(
                "RTT variation: the mean absolute difference between the round trips of two probes sent one after the other, both answered. A pair is dropped if anything lies between them — an unanswered probe, a skipped slot, a gap, a change of destination. Needs {pairs} pairs. It is a round-trip quantity sampled at the probe interval: not the one-way delay variation of RFC 3393 and not the jitter of RFC 3550."
            ),
        ),
        term("n", Times, "how many round trips the times above rest on", "The number of answered round trips behind every time shown for that stream and window."),
        term(
            "no-reply",
            Shares,
            "(timed out + unreachable) ÷ finalised; not proof of loss",
            format!(
                "The share of finalised probes that got no answer: timed out or reported unreachable, over everything that was sent and has reached an outcome. A percentage from {rate} finalised probes; below that, the count. It is not proof of packet loss on your line: the request may have been lost, or the reply, or the target may limit how often it answers, or the answer may have come after the deadline."
            ),
        ),
        term(
            "err-resp",
            Shares,
            "answers that were errors or not as expected ÷ finalised",
            format!(
                "The share of finalised probes that were answered, but not as expected: an error code, an answer the query's expectation does not allow, a blocked answer, or bytes that are not a valid answer to the question. An answer arrived, so none of these is a timeout. A percentage from {rate} finalised probes; below that, the count."
            ),
        ),
        term(
            "cov",
            Shares,
            "probes sent ÷ probes the window has room for",
            "Coverage: the probes sent within the window over the number of probe intervals the window holds, at most 100 %. A young session, a gap, a stop, or skipped slots lower it. It says how much of the window the other numbers speak for.",
        ),
        term("sent", Counts, "probes that left this host", "Probes handed to the network: pending, replied, timed out, unreachable, error responses and cancelled together."),
        term("pending", Counts, "sent, and the deadline has not passed yet", "Sent and still waiting: neither answered nor past the deadline. Not counted in any share."),
        term("replied", Counts, "answered as expected, in time", "Answered before the deadline with what was expected. Only these have a round-trip time in the statistics."),
        term(
            "timed out",
            Counts,
            "no answer before the deadline",
            "Nothing came back before the deadline. A reply that arrives afterwards is noted as late and the probe stays timed out.",
        ),
        term(
            "unreachable",
            Counts,
            "refused, or reported unreachable on the way",
            "An explicit refusal instead of an answer: a router's destination-unreachable message, a closed port, a refused connection.",
        ),
        term(
            "error responses",
            Counts,
            "an answer came, and it was an error or unexpected",
            "Answered, but with an error code, an answer the expectation does not allow, or something that is not a valid answer. The numerator of err-resp.",
        ),
        term(
            "local errors",
            Counts,
            "this host could not send; nothing left it",
            "The send failed on this host — no route, no address of that family, permission refused. Nothing reached the network, so these are never counted as unanswered.",
        ),
        term(
            "skipped",
            Counts,
            "a slot that was not sent, on purpose",
            "A probe slot that was deliberately not used: the rate limit was reached, results were not being taken fast enough, or the timer fired too late for the slot to mean anything. Never counted as unanswered.",
        ),
        term(
            "cancelled",
            Counts,
            "in flight when the program stopped watching",
            "Sent, and then the program could not vouch for the wait: the host slept, the network changed, collection was stopped, or the session ended. Not counted as unanswered.",
        ),
        term(
            "late",
            Counts,
            "replies that came after their probe had timed out",
            "Replies that arrived after the deadline, within the late window. Each is recorded with its time; the probe it belongs to remains timed out.",
        ),
        term("duplicate", Counts, "extra replies to a probe already answered", "Further replies to a probe that had its answer. The first one's time stands."),
        term(
            "unanswered run",
            Counts,
            "probes in a row with no answer: longest, and current",
            "The longest run of consecutive probes with no answer in the window, and the run in progress. Any answer ends a run, an error response included; so does a gap.",
        ),
        term(
            "OK",
            Status,
            "answering, with nothing open against it",
            "The stream is answering and no rule below has opened. It is drawn in the ordinary text colour, not in green.",
        ),
        term(
            "WARMING UP",
            Status,
            format!("fewer than {rate} finalised probes: too young to judge"),
            format!(
                "Nothing has been sent yet, or fewer than {rate} probes have been finalised this session and nothing is open against the stream. A stream that probes once a minute stays here for {rate} minutes."
            ),
        ),
        term(
            "NO REPLY",
            Status,
            format!("{} probes in a row unanswered ({} for DNS)", d.run_open, d.dns_run_open),
            format!(
                "{} consecutive probes went unanswered ({} on a DNS stream). It ends after {} consecutive answers ({} on a DNS stream).",
                d.run_open, d.dns_run_open, d.run_close, d.dns_run_close
            ),
        ),
        term(
            "LOSSY",
            Status,
            format!("{:.0} % or more unanswered over the last minute", d.elevated_open * 100.0),
            format!(
                "{:.0} % or more of the probes of the last {} s went unanswered, out of at least {}. It ends when the share has stayed at or below {:.0} % for {} s. The word describes the stream's answers, not your line.",
                d.elevated_open * 100.0,
                secs(d.elevated_window_ns),
                d.elevated_min,
                d.elevated_close * 100.0,
                secs(d.elevated_hold_ns)
            ),
        ),
        term(
            "SLOW",
            Status,
            format!("last minute's median over {}× its baseline", d.shift_open_ratio),
            format!(
                "The median of the last {} s is more than {} times the median of the {} minutes before it, and at least {:.0} ms above it. It needs {} recent answers and {} in the baseline, so it is only ever said of a stream that has been running a while. The baseline is frozen while it is open; it ends when the median has stayed below {} times the baseline for {} s.",
                secs(d.shift_recent_ns),
                d.shift_open_ratio,
                secs(d.shift_baseline_ns) / 60,
                d.shift_open_min_delta_us / 1000.0,
                d.shift_min_recent,
                d.shift_min_baseline,
                d.shift_close_ratio,
                secs(d.shift_recent_ns)
            ),
        ),
        term(
            "ERRORS",
            Status,
            format!("{} of the last 10 DNS answers were errors", d.dns_errors_in_last_10),
            format!(
                "{} of the last 10 DNS transactions on the stream were error responses. Answers arrived; these are not timeouts. It ends after 10 consecutive transactions answered as expected.",
                d.dns_errors_in_last_10
            ),
        ),
        term(
            "LOCAL ERR",
            Status,
            "sends fail on this host; nothing reached the network",
            "Sending is failing on this host. Nothing reached the network, so nothing is said about the target and nothing is counted as unanswered. It ends with the first send that works.",
        ),
        term(
            "STALE",
            Status,
            "no result for three probe intervals",
            "The stream should be producing results and has not for three probe intervals (five seconds at least) plus one deadline. The numbers shown are old.",
        ),
        term(
            "NEEDS CONFIG",
            Status,
            "not configured yet; nothing is sent",
            "The provider or target is waiting for a value only you have. Nothing is sent, and nothing is counted as unanswered.",
        ),
        term(
            "UNSUPPORTED",
            Status,
            "this host cannot do it; nothing is sent",
            "This host does not permit the probe, or the target has no endpoint for this protocol. The reason is shown. Nothing is sent, and it is never reported as the target failing.",
        ),
        term("DISABLED", Status, "switched off in the configuration", "Switched off in the configuration, or left out by the mode or address family in use."),
        term(
            "—",
            Marks,
            "too few samples to show a number",
            "A dash in place of a number: there are too few samples for that statistic to be shown at all. In ASCII mode it is drawn as -.",
        ),
        term(
            "·",
            Marks,
            format!("after a number: it rests on fewer than {low} samples"),
            format!("A raised dot after a number: the number is shown, and rests on fewer than {low} samples. In ASCII mode it is drawn as a full stop."),
        ),
        term(
            "outcome strip",
            Marks,
            "· answered  ▒ some unanswered  █ none answered",
            "The row of marks under a chart, one per column of time: · every probe answered, ▒ some unanswered, █ none answered, E an error response, ! a local error, ┄ skipped, ╌ a gap in observation, blank where nothing was due. In ASCII mode: . o # E ! _ ~.",
        ),
    ]
}

/// The glossary as plain text: no table, no symbols standing in for words,
/// one term per paragraph.
pub fn text() -> String {
    let mut out = String::from("What pathwatch's words and numbers mean\n");
    for group in Group::ALL {
        out.push_str(&format!("\n{}\n{}\n", group.title().to_uppercase(), group.preface()));
        for t in terms().iter().filter(|t| t.group == group) {
            out.push_str(&format!("\n{}\n  {}\n", t.label, t.detail));
        }
    }
    out
}

/// The glossary as Markdown, for the documentation.
pub fn markdown() -> String {
    let mut out = String::from(
        "# Glossary\n\nWhat every word and number `pathwatch` shows means, and what it does not. Generated from the program by `pathwatch glossary --markdown`; do not edit by hand.\n",
    );
    for group in Group::ALL {
        out.push_str(&format!("\n## {}\n\n{}\n\n| Shown as | Meaning |\n|---|---|\n", group.title(), group.preface()));
        for t in terms().iter().filter(|t| t.group == group) {
            out.push_str(&format!("| `{}` | {} |\n", t.label, t.detail.replace('|', "\\|")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Status;

    #[test]
    fn every_term_fits_the_help_panel_and_is_explained_once() {
        let all = terms();
        let mut seen = std::collections::BTreeSet::new();
        for t in &all {
            assert!(t.label.chars().count() <= LABEL_WIDTH, "{:?} is too long a label", t.label);
            assert!(t.short.chars().count() <= SHORT_WIDTH, "{:?}: {:?} is {} characters", t.label, t.short, t.short.chars().count());
            assert!(!t.short.is_empty() && t.detail.ends_with('.') && t.detail.len() > t.short.len(), "{:?}", t.label);
            assert!(seen.insert(t.label), "{:?} twice", t.label);
        }
        for group in Group::ALL {
            assert!(all.iter().any(|t| t.group == group), "{group:?} is empty");
        }
    }

    #[test]
    fn every_status_the_dashboard_can_print_is_in_the_glossary_and_no_other() {
        let printed: std::collections::BTreeSet<&str> = [
            Status::Ok,
            Status::WarmingUp,
            Status::Lossy,
            Status::Slow,
            Status::Errors,
            Status::NoReply,
            Status::LocalErr,
            Status::Stale,
            Status::NeedsConfig(String::new()),
            Status::Unsupported(String::new()),
            Status::Disabled,
        ]
        .iter()
        .map(Status::word)
        .collect();
        let explained: std::collections::BTreeSet<&str> = terms().iter().filter(|t| t.group == Group::Status).map(|t| t.label).collect();
        assert_eq!(printed, explained);
    }

    #[test]
    fn the_thresholds_quoted_are_the_ones_in_force() {
        let find = |label: &str| terms().into_iter().find(|t| t.label == label).unwrap_or_else(|| panic!("{label}"));
        assert_eq!(find("p50").short, "the median round trip; needs 5 answers");
        assert_eq!(find("p95").short, "the 95th percentile, nearest rank; needs 20 answers");
        assert_eq!(find("NO REPLY").short, "5 probes in a row unanswered (3 for DNS)");
        assert_eq!(find("LOSSY").short, "5 % or more unanswered over the last minute");
        assert_eq!(find("SLOW").short, "last minute's median over 1.5× its baseline");
        assert!(find("SLOW").detail.contains("the 15 minutes before it, and at least 10 ms above it") && find("SLOW").detail.contains("30 recent answers and 300 in the baseline"));
        assert!(find("LOSSY").detail.contains("of the last 60 s") && find("LOSSY").detail.contains("at or below 1 % for 30 s"));
        assert!(find("no-reply").detail.contains("A percentage from 20 finalised probes"));
    }

    #[test]
    fn both_renderings_carry_every_term() {
        let (plain, md) = (text(), markdown());
        for t in terms() {
            assert!(plain.contains(&format!("\n{}\n  {}", t.label, t.detail)), "{:?} in the text", t.label);
            assert!(md.contains(&format!("| `{}` |", t.label)), "{:?} in the Markdown", t.label);
        }
        // A table row is one line, whatever the text holds.
        assert!(md.lines().filter(|l| l.starts_with("| `")).all(|l| l.ends_with(" |")));
    }
}
