//! Grouping incidents that open together on comparable streams.
//!
//! When several streams of the same protocol and address family run into
//! trouble within a few seconds of each other, that is one thing to tell the
//! user, not several — and the useful half of it is which comparable streams
//! were *not* affected. A group is described with counts taken from the
//! observations; it never names a cause.
//!
//! This is a pure function of the incident list and the statistics at a
//! point in time, so a frozen view, a replay, and a live session all group
//! the same way.

use std::time::Duration;

use crate::engine::Engine;
use crate::incident::{IncidentKind, IncidentTracker, Rule};
use crate::model::{Family, Protocol};
use crate::snapshot::{StreamMeta, StreamState};
use crate::stats::Gated;

/// Incidents opening within this long of a group's first one belong to it.
pub const WINDOW: Duration = Duration::from_secs(10);
/// What an affected or unaffected stream is described over.
const LOOKBACK: Duration = Duration::from_secs(60);

/// Kinds of trouble that can be meaningfully counted together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Probes going unanswered: a run, or an elevated rate.
    Unanswered,
    /// The recent median well above its baseline.
    Slow,
    /// Error responses from the service.
    Errors,
    /// Sends refused by this host. Nothing reached the network.
    Local,
}

impl Class {
    fn of(rule: Rule) -> Self {
        match rule {
            Rule::NoReplyRun | Rule::ElevatedNoReply => Class::Unanswered,
            Rule::LatencyShift => Class::Slow,
            Rule::ErrorResponses => Class::Errors,
            Rule::LocalError => Class::Local,
        }
    }

    fn rules(self) -> &'static [Rule] {
        match self {
            Class::Unanswered => &[Rule::NoReplyRun, Rule::ElevatedNoReply],
            Class::Slow => &[Rule::LatencyShift],
            Class::Errors => &[Rule::ErrorResponses],
            Class::Local => &[Rule::LocalError],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// When the first member opened, and the last.
    pub at_ns: u64,
    pub last_ns: u64,
    pub utc: Option<String>,
    pub class: Class,
    /// `None` for [`Class::Local`], which spans protocols: a host that
    /// cannot send over IPv6 cannot send any protocol over it.
    pub protocol: Option<Protocol>,
    pub family: Family,
    /// Indices into the tracker's event list that this group stands for.
    pub events: Vec<usize>,
    /// Streams in the group, and comparable streams outside it.
    pub affected: Vec<usize>,
    pub unaffected: Vec<usize>,
    /// Short label for the event list: `3 of 4 icmp v4`.
    pub label: String,
    /// The whole finding in words, assembled from counts.
    pub text: String,
}

fn protocol_name(p: Protocol) -> &'static str {
    match p {
        Protocol::Icmp => "ICMP",
        Protocol::DnsUdp => "DNS/UDP",
        Protocol::DnsTcp => "DNS/TCP",
        Protocol::Dot => "DoT",
        Protocol::Doh => "DoH",
    }
}

fn median(g: Gated) -> Option<f64> {
    match g {
        Gated::Ok(v) | Gated::LowN(v) => Some(v / 1000.0),
        Gated::Insufficient => None,
    }
}

/// Finds the groups among the incidents that had opened by `as_of_ns`.
pub fn groups(engine: &Engine, incidents: &IncidentTracker, streams: &[StreamMeta], as_of_ns: u64) -> Vec<Group> {
    let window = WINDOW.as_nanos() as u64;
    // A stream can be grouped with others when it is collecting often
    // enough to say something about a ten-second window. A "fresh
    // connection" stream probes every few minutes and cannot; nor can any
    // other stream whose interval is longer than the window, which is what
    // DNS over TCP is unless it has been set to probe faster.
    // A control is not one of the things being compared; it is what they
    // are compared against.
    let frequent = |m: &StreamMeta| engine.stream_info(m.idx).interval <= WINDOW;
    let comparable = |m: &StreamMeta| m.state == StreamState::Collecting && !m.fresh && frequent(m) && !crate::localnet::is_control(&m.provider);
    let key = |m: &StreamMeta, class: Class| if class == Class::Local { None } else { Some(m.protocol) };
    let meta = |idx: usize| streams.iter().find(|m| m.idx == idx);

    // Openings, oldest first, of streams that can be compared at all.
    let mut opens: Vec<(usize, u64, usize, Class)> = incidents
        .events()
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == IncidentKind::Opened && e.at_ns <= as_of_ns && meta(e.stream).is_some_and(comparable))
        .map(|(i, e)| (i, e.at_ns, e.stream, Class::of(e.rule)))
        .collect();
    opens.sort_by_key(|o| o.1);

    let mut out = Vec::new();
    let mut used = vec![false; opens.len()];
    for a in 0..opens.len() {
        if used[a] {
            continue;
        }
        let (_, first, anchor, class) = opens[a];
        let Some(anchor) = meta(anchor) else { continue };
        let (protocol, family) = (key(anchor, class), anchor.family);
        let alike = |m: &StreamMeta| comparable(m) && m.family == family && key(m, class) == protocol;
        // Everything of the same kind, on a comparable stream, within the
        // window of the first. Anchored to the first so a group cannot creep.
        let members: Vec<usize> = (a..opens.len()).filter(|&b| !used[b] && opens[b].3 == class && opens[b].1 - first <= window && meta(opens[b].2).is_some_and(alike)).collect();
        let mut affected: Vec<usize> = members.iter().map(|&b| opens[b].2).collect();
        affected.sort_unstable();
        affected.dedup();
        if affected.len() < 2 {
            continue; // one stream, however many rules it tripped, is not a correlation
        }
        for &b in &members {
            used[b] = true;
        }
        let last = members.iter().map(|&b| opens[b].1).max().unwrap_or(first);

        // The comparable streams outside the group, as they stood when its
        // last member opened.
        let open_then = |idx: usize| class.rules().iter().any(|r| incidents.is_open_at(idx, *r, last));
        let (mut unaffected, mut already, mut silent) = (Vec::new(), Vec::new(), Vec::new());
        for m in streams.iter().filter(|m| alike(m) && !affected.contains(&m.idx)) {
            if open_then(m.idx) {
                already.push(m.idx);
            } else if engine.stats(m.idx, LOOKBACK, last + 1).slots == 0 {
                silent.push(m.idx);
            } else {
                unaffected.push(m.idx);
            }
        }

        // Names: the provider, with the target (or the protocol, for a group
        // across protocols) only where the provider alone would be ambiguous.
        let all: Vec<usize> = affected.iter().chain(&unaffected).chain(&already).chain(&silent).copied().collect();
        let name = |idx: usize| -> String {
            let Some(m) = meta(idx) else { return String::new() };
            let id = engine.stream_info(idx).id.clone();
            let target = id.split('/').nth(1).unwrap_or("");
            let same_provider = all.iter().filter_map(|i| meta(*i)).filter(|o| o.provider == m.provider);
            match protocol {
                None => format!("{} {}", m.provider, protocol_name(m.protocol)),
                Some(_) if same_provider.count() > 1 => format!("{}/{target}", m.provider),
                Some(_) => m.provider.to_string(),
            }
        };
        // What each stream's own observations showed in the minute up to and
        // including `at` — the same probes its incident was opened on.
        let detail = |idx: usize, at: u64| -> String {
            let s = engine.stats(idx, LOOKBACK, at + 1);
            match class {
                Class::Unanswered => format!("{} of {} unanswered", s.unanswered, s.finalised),
                Class::Errors => format!("{} of {} error responses", s.errors, s.finalised),
                Class::Slow => median(s.p50_us).map_or_else(|| "too few replies for a median".into(), |v| format!("median {v:.1} ms")),
                Class::Local => format!("{} of {} sends refused", s.local_error, s.slots),
            }
        };
        let opened_at = |idx: usize| members.iter().map(|&b| opens[b]).filter(|o| o.2 == idx).map(|o| o.1).min().unwrap_or(first);
        let list = |ids: &[usize], at: &dyn Fn(usize) -> u64| ids.iter().map(|&i| format!("{} ({})", name(i), detail(i, at(i)))).collect::<Vec<_>>().join(", ");

        let total = all.len();
        let kind = match protocol {
            Some(p) => format!("{} v{}", protocol_name(p), family.as_str()),
            None => format!("IPv{}", family.as_str()),
        };
        let began = match class {
            Class::Unanswered => "began going unanswered",
            Class::Slow => "slowed",
            Class::Errors => "began returning errors",
            Class::Local => "had sends refused by this host",
        };
        // How far apart the streams first ran into it — not how long each
        // went on tripping further rules afterwards.
        let spread = affected.iter().map(|&i| opened_at(i)).max().unwrap_or(first) - first;
        let secs = spread.div_ceil(1_000_000_000).max(1);
        let mut text = format!("{} of {total} {kind} streams {began} within {secs} s of each other: {}.", affected.len(), list(&affected, &opened_at));
        let fine = match class {
            Class::Unanswered => "Still answering",
            Class::Slow => "Not slowed",
            Class::Errors => "Without errors",
            Class::Local => "Still sending",
        };
        if !unaffected.is_empty() {
            text.push_str(&format!(" {fine}: {}.", list(&unaffected, &|_| last)));
        }
        if !already.is_empty() {
            text.push_str(&format!(" Already affected before this: {}.", already.iter().map(|&i| name(i)).collect::<Vec<_>>().join(", ")));
        }
        if !silent.is_empty() {
            text.push_str(&format!(" No probes in that minute: {}.", silent.iter().map(|&i| name(i)).collect::<Vec<_>>().join(", ")));
        }
        if unaffected.is_empty() && already.is_empty() {
            text.push_str(" No comparable stream was unaffected.");
        }
        // The controls answer "was the local segment fine at that moment?".
        // When none is running, say so rather than leave it to be assumed.
        text.push_str(&controls_text(engine, streams, family, last));
        if class == Class::Local {
            text.push_str(" Nothing reached the network; this is not counted as loss.");
        }

        out.push(Group {
            at_ns: first,
            last_ns: last,
            utc: incidents.events()[opens[a].0].utc.clone().or_else(|| incidents.utc_at(first)),
            class,
            protocol,
            family,
            events: members.iter().map(|&b| opens[b].0).collect(),
            label: format!("{} of {total} {}", affected.len(), kind.to_lowercase()),
            affected,
            unaffected,
            text,
        });
    }
    out
}

/// What the controls of this address family showed in the minute up to `at`.
fn controls_text(engine: &Engine, streams: &[StreamMeta], family: Family, at: u64) -> String {
    use crate::localnet::{GATEWAY, is_control};
    let running: Vec<&StreamMeta> = streams.iter().filter(|m| is_control(&m.provider) && m.state == StreamState::Collecting && m.family == family).collect();
    if running.is_empty() {
        return " No gateway or local-DNS control is configured.".into();
    }
    let parts: Vec<String> = running
        .iter()
        .map(|m| {
            // What the resolver's answers can speak for depends on where it
            // is: a stub on this host, the local network, or the internet.
            let dest: Option<std::net::IpAddr> = engine.latest_record(m.idx).and_then(|o| o.dest_ip).or_else(|| m.configured.parse().ok());
            let name = match (&*m.provider == GATEWAY, dest.map(crate::localnet::ResolverKind::of)) {
                (true, _) => "gateway",
                (false, Some(kind)) => kind.cited(),
                (false, None) => "local DNS",
            };
            let s = engine.stats(m.idx, LOOKBACK, at + 1);
            if s.finalised == 0 {
                return format!("{name} had no finalised probe");
            }
            // A gateway that has never answered says nothing either way.
            let never = &*m.provider == GATEWAY && !engine.answered_before(m.idx, at + 1);
            let caveat = if never { " (it has not answered an echo this session; some gateways never do)" } else { "" };
            format!("{name} {} of {} answered{caveat}", s.finalised - s.unanswered, s.finalised)
        })
        .collect();
    format!(" Controls in the same minute: {}.", parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::HistoryLimits;
    use crate::model::{ErrorCategory, Msg, Record};
    use crate::script::{self, Behaviour, Scenario, four_icmp, rule, stream};

    struct Played {
        engine: Engine,
        incidents: IncidentTracker,
        streams: Vec<StreamMeta>,
    }

    fn play(sc: &Scenario) -> Played {
        let (_, streams) = sc.metas();
        let mut engine = Engine::new(sc.infos(), HistoryLimits::default());
        let mut incidents = IncidentTracker::new(sc.streams.iter().map(|s| s.info.protocol));
        for m in script::generate(sc, 7) {
            engine.ingest(&m);
            match &m {
                Msg::Record(Record::Observation(o)) => incidents.on_observation(engine.stream_index(&o.stream).unwrap(), o),
                Msg::Record(Record::Gap(_)) => incidents.on_gap(),
                _ => {}
            }
        }
        Played { engine, incidents, streams }
    }

    fn scenario(streams: Vec<script::ScriptStream>) -> Scenario {
        Scenario { name: "fixture", shows: "", duration: Duration::from_secs(150), streams, gaps: Vec::new() }
    }

    /// Words that would claim a cause. A group reports counts; it does not diagnose.
    const ATTRIBUTION: [&str; 10] = ["outage", "your line", "isp", "provider is", "is down", "because", "caused", "fault", "confirmed", "packet loss"];

    fn assert_no_attribution(text: &str) {
        let lower = text.to_lowercase();
        for word in ATTRIBUTION {
            assert!(!lower.contains(word), "{word:?} in: {text}");
        }
    }

    /// Three of four ICMP targets stop at 60 s; the fourth and a DNS stream carry on.
    fn three_of_four() -> Scenario {
        let mut s = four_icmp();
        for st in s.iter_mut().take(3) {
            st.rules.push(rule(60, 100, Behaviour::Silent));
        }
        s.push(stream("cloudflare", "resolver-v4", Protocol::DnsUdp, Family::V4, "1.1.1.1", 14, 900, 100));
        scenario(s)
    }

    #[test]
    fn a_correlated_failure_is_one_event_listing_who_was_and_was_not_affected() {
        let p = play(&three_of_four());
        let groups = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        assert_eq!(groups.len(), 1, "{groups:#?}");
        let g = &groups[0];
        assert_eq!((g.class, g.protocol, g.family), (Class::Unanswered, Some(Protocol::Icmp), Family::V4));
        assert_eq!((g.affected.clone(), g.unaffected.clone()), (vec![0, 1, 2], vec![3]), "the DNS stream is not comparable to ICMP ones");
        assert_eq!(g.label, "3 of 4 icmp v4");
        assert!(g.last_ns - g.at_ns <= WINDOW.as_nanos() as u64);

        // Counts, for the affected and for the unaffected, and no cause.
        let t = &g.text;
        assert!(t.starts_with("3 of 4 ICMP v4 streams began going unanswered within "), "{t}");
        for who in ["cloudflare (", "google (", "nextdns ("] {
            assert!(t.contains(who), "{who} in {t}");
        }
        assert!(t.matches(" unanswered)").count() == 4 && t.contains("Still answering: arelion (0 of 60 unanswered)."), "{t}");
        assert!(t.contains("No gateway or local-DNS control is configured."), "the absence of controls is stated: {t}");
        assert_no_attribution(t);

        // It stands for every opening it groups: both rules of each of the three streams.
        let grouped: Vec<_> = g.events.iter().map(|&i| &p.incidents.events()[i]).collect();
        assert!(grouped.len() >= 3 && grouped.iter().all(|e| e.kind == IncidentKind::Opened && e.stream < 3), "{grouped:?}");
        let opened_in_window =
            p.incidents.events().iter().filter(|e| e.kind == IncidentKind::Opened && e.at_ns >= g.at_ns && e.at_ns <= g.at_ns + WINDOW.as_nanos() as u64).count();
        assert_eq!(grouped.len(), opened_in_window, "none of them is left to be listed on its own");
    }

    #[test]
    fn controls_are_cited_with_their_counts_and_never_counted_among_the_streams() {
        let control = |provider: &'static str, protocol, phase| stream(provider, "auto", protocol, Family::V4, "192.168.1.1", 2, 300, phase);
        // The gateway and the local resolver answer throughout.
        let mut sc = three_of_four();
        sc.streams.push(control(crate::localnet::GATEWAY, Protocol::Icmp, 333));
        sc.streams.push(control(crate::localnet::LOCAL_DNS, Protocol::DnsUdp, 1_666));
        let p = play(&sc);
        let found = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        assert_eq!(found.len(), 1, "{found:#?}");
        let t = &found[0].text;
        assert!(t.starts_with("3 of 4 ICMP v4 streams"), "the gateway is ICMP v4 too, and is not one of the four: {t}");
        assert!(t.contains("Controls in the same minute: gateway 60 of 60 answered; local DNS 12 of 12 answered."), "{t}");
        assert!(!t.contains("No gateway or local-DNS control"));
        assert_no_attribution(t);

        // The gateway stops answering with them: it is reported, not grouped,
        // and the count speaks for itself (five echoes unanswered by the
        // time the last of the three streams opened its incident).
        let mut sc = three_of_four();
        let mut gw = control(crate::localnet::GATEWAY, Protocol::Icmp, 333);
        gw.rules.push(rule(60, 100, Behaviour::Silent));
        sc.streams.push(gw);
        let p = play(&sc);
        let found = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        assert_eq!(found.len(), 1);
        assert!(found[0].label == "3 of 4 icmp v4" && found[0].text.contains("gateway 55 of 60 answered"), "{}", found[0].text);
        assert_no_attribution(&found[0].text);

        // A gateway that has never answered an echo carries its caveat.
        let mut sc = three_of_four();
        let mut gw = control(crate::localnet::GATEWAY, Protocol::Icmp, 333);
        gw.rules.push(rule(0, 150, Behaviour::Silent));
        sc.streams.push(gw);
        let p = play(&sc);
        let t = &groups(&p.engine, &p.incidents, &p.streams, u64::MAX)[0].text;
        assert!(t.contains("gateway 0 of 60 answered (it has not answered an echo this session; some gateways never do)"), "{t}");
    }

    #[test]
    fn a_resolver_across_the_internet_is_not_called_local() {
        // As on the network of the first live run with controls: the system's
        // resolver is a public address, so its answers say nothing about the
        // local segment — and the event must not suggest they do.
        let mut sc = three_of_four();
        sc.streams.push(stream(crate::localnet::GATEWAY, "auto", Protocol::Icmp, Family::V4, "192.168.1.1", 2, 300, 333));
        sc.streams.push(stream(crate::localnet::LOCAL_DNS, "auto", Protocol::DnsUdp, Family::V4, "203.0.113.53", 30, 900, 1_666));
        let p = play(&sc);
        let t = &groups(&p.engine, &p.incidents, &p.streams, u64::MAX)[0].text;
        assert!(t.contains("gateway 60 of 60 answered; system DNS (a remote resolver, not on the local network) 12 of 12 answered."), "{t}");
        assert!(!t.contains("local DNS"), "{t}");
        assert_no_attribution(t);

        // A resolver on the local network keeps the plain name.
        let mut sc = three_of_four();
        sc.streams.push(stream(crate::localnet::LOCAL_DNS, "auto", Protocol::DnsUdp, Family::V4, "192.168.1.1", 2, 300, 1_666));
        let p = play(&sc);
        let t = &groups(&p.engine, &p.incidents, &p.streams, u64::MAX)[0].text;
        assert!(t.contains("Controls in the same minute: local DNS 12 of 12 answered."), "{t}");
    }

    #[test]
    fn when_every_comparable_stream_is_affected_it_says_so() {
        let p = play(&script::by_name("correlated-outage").unwrap());
        let groups = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        let icmp: Vec<_> = groups.iter().filter(|g| g.protocol == Some(Protocol::Icmp)).collect();
        assert_eq!(icmp.len(), 1, "{groups:#?}");
        assert_eq!(icmp[0].label, "4 of 4 icmp v4");
        assert!(icmp[0].text.contains("No comparable stream was unaffected."), "{}", icmp[0].text);
        // One DNS stream of two went silent: that is a single stream, not a group.
        assert!(groups.iter().all(|g| g.protocol != Some(Protocol::DnsUdp)), "{groups:#?}");
        groups.iter().for_each(|g| assert_no_attribution(&g.text));
    }

    #[test]
    fn one_stream_alone_or_two_far_apart_are_not_a_group() {
        // One target, tripping two rules within seconds of each other.
        let p = play(&script::by_name("isolated-failure").unwrap());
        assert!(p.incidents.events().iter().filter(|e| e.kind == IncidentKind::Opened).count() >= 2);
        assert_eq!(groups(&p.engine, &p.incidents, &p.streams, u64::MAX), Vec::new());

        // Two targets failing half a minute apart.
        let mut s = four_icmp();
        s[0].rules.push(rule(40, 60, Behaviour::Silent));
        s[1].rules.push(rule(70, 90, Behaviour::Silent));
        let p = play(&scenario(s));
        assert!(p.incidents.events().iter().filter(|e| e.kind == IncidentKind::Opened).map(|e| e.stream).collect::<std::collections::BTreeSet<_>>().len() == 2);
        assert_eq!(groups(&p.engine, &p.incidents, &p.streams, u64::MAX), Vec::new());
    }

    #[test]
    fn a_group_exists_only_once_its_second_member_has_opened() {
        let p = play(&three_of_four());
        let g = &groups(&p.engine, &p.incidents, &p.streams, u64::MAX)[0];
        let second = {
            let mut firsts: Vec<u64> =
                g.affected.iter().map(|&s| p.incidents.events().iter().filter(|e| e.stream == s && e.kind == IncidentKind::Opened).map(|e| e.at_ns).min().unwrap()).collect();
            firsts.sort_unstable();
            firsts[1]
        };
        // A view frozen between the first and second openings shows one stream's incident, ungrouped.
        assert_eq!(groups(&p.engine, &p.incidents, &p.streams, second - 1), Vec::new());
        let then = groups(&p.engine, &p.incidents, &p.streams, second);
        assert_eq!(then.len(), 1);
        assert!(then[0].affected.len() == 2 && then[0].label == "2 of 4 icmp v4", "{:?}", then[0]);
    }

    #[test]
    fn a_stream_already_in_trouble_is_named_as_such_not_as_unaffected() {
        let mut s = four_icmp();
        s[3].rules.push(rule(10, 140, Behaviour::Silent));
        s[0].rules.push(rule(80, 110, Behaviour::Silent));
        s[1].rules.push(rule(80, 110, Behaviour::Silent));
        let p = play(&scenario(s));
        let groups = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        assert_eq!(groups.len(), 1, "{groups:#?}");
        let t = &groups[0].text;
        assert!(t.starts_with("2 of 4 ICMP v4 streams"), "{t}");
        assert!(t.contains("Still answering: nextdns (0 of 60 unanswered).") && t.contains("Already affected before this: arelion."), "{t}");
        assert!(!t.contains("No comparable stream was unaffected"));
    }

    #[test]
    fn streams_that_probe_less_often_than_the_window_are_not_grouped() {
        // Two DNS/TCP streams stop being answered at the same moment.
        let pair = |every: u64| {
            let tcp = |provider: &'static str, ip: &'static str, phase| {
                let mut st = stream(provider, "resolver-v4", Protocol::DnsTcp, Family::V4, ip, 30, 900, phase);
                st.info.interval = Duration::from_secs(every);
                st.rules.push(rule(60, 600, Behaviour::Silent));
                st
            };
            Scenario {
                name: "fixture",
                shows: "",
                duration: Duration::from_secs(600),
                streams: vec![tcp("cloudflare", "1.1.1.1", 100), tcp("google", "8.8.8.8", 1_300)],
                gaps: Vec::new(),
            }
        };
        // Once a minute: each opens an incident of its own, on three probes
        // spread over three minutes. That is too little to call one event.
        let p = play(&pair(60));
        let opened = p.incidents.events().iter().filter(|e| e.kind == IncidentKind::Opened).count();
        assert_eq!(opened, 2, "{:#?}", p.incidents.events());
        assert_eq!(groups(&p.engine, &p.incidents, &p.streams, u64::MAX), Vec::new());

        // Set to probe every five seconds, the same two can be compared.
        let p = play(&pair(5));
        let found = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!((found[0].protocol, found[0].label.as_str()), (Some(Protocol::DnsTcp), "2 of 2 dns/tcp v4"));
        assert!(found[0].text.starts_with("2 of 2 DNS/TCP v4 streams began going unanswered within "), "{}", found[0].text);
        assert_no_attribution(&found[0].text);
    }

    #[test]
    fn error_responses_and_refused_sends_are_grouped_in_their_own_words() {
        // Two DNS resolvers return SERVFAIL together; a third does not.
        let dns = |provider: &'static str, ip: &'static str, phase| stream(provider, "resolver-v4", Protocol::DnsUdp, Family::V4, ip, 14, 900, phase);
        let mut s = vec![dns("cloudflare", "1.1.1.1", 100), dns("google", "8.8.8.8", 1_300), dns("nextdns", "203.0.113.28", 2_500)];
        for st in s.iter_mut().take(2) {
            st.rules.push(rule(60, 120, Behaviour::ServiceError(ErrorCategory::DnsRcode, "SERVFAIL")));
        }
        let p = play(&scenario(s));
        let found = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        assert_eq!(found.len(), 1, "{found:#?}");
        let t = &found[0].text;
        assert!(t.starts_with("2 of 3 DNS/UDP v4 streams began returning errors within "), "{t}");
        assert!(t.contains(" error responses)") && t.contains("Without errors: nextdns (0 of 12 error responses)."), "{t}");
        assert_no_attribution(t);

        // No IPv6 route: every IPv6 stream is refused at once, whatever its
        // protocol, while IPv4 carries on. One finding, across protocols.
        let v6 = |provider: &'static str, protocol, phase| {
            let mut st = stream(provider, "resolver-v6", protocol, Family::V6, "2001:db8::1", 14, 900, phase);
            st.rules.push(rule(0, 150, Behaviour::LocalError(ErrorCategory::NoRoute)));
            st
        };
        let mut s = four_icmp();
        s.extend([v6("cloudflare", Protocol::Icmp, 0), v6("google", Protocol::Icmp, 300), v6("cloudflare", Protocol::DnsUdp, 600), v6("google", Protocol::Doh, 900)]);
        let p = play(&scenario(s));
        let found = groups(&p.engine, &p.incidents, &p.streams, u64::MAX);
        assert_eq!(found.len(), 1, "{found:#?}");
        let g = &found[0];
        assert_eq!((g.class, g.protocol, g.family, g.label.as_str()), (Class::Local, None, Family::V6, "4 of 4 ipv6"));
        assert!(g.text.starts_with("4 of 4 IPv6 streams had sends refused by this host within "), "{}", g.text);
        assert!(
            g.text.contains("cloudflare ICMP (") && g.text.contains("google DoH (") && g.text.contains("Nothing reached the network; this is not counted as loss."),
            "{}",
            g.text
        );
        assert_no_attribution(&g.text);
    }
}
