# Glossary

What every word and number `pathwatch` shows means, and what it does not. Generated from the program by `pathwatch glossary --markdown`; do not edit by hand.

## Times

In milliseconds, from answered probes sent within the window, to the current destination only. An ICMP round trip ends at the kernel's receive stamp; DNS, DoT and DoH end at this program's read. A fresh or dns-tcp stream includes setting up its connection.

| Shown as | Meaning |
|---|---|
| `last` | The round-trip time of the most recent probe that was answered as expected. It is the newest answer in the session, not in the window, so it can be older than the window; a stream that has stopped answering keeps showing its last one. |
| `p50` | The median of the answered round trips, by nearest rank: the value halfway up the sorted times, rounding the rank up, so always a time that was actually measured. Withheld below 5 answers, and marked as resting on few below 20. |
| `p95` | The time that 95 % of answered round trips did not exceed, by nearest rank. Withheld below 20 answers: with fewer, it would silently be the maximum. |
| `p99` | As p95, for 99 %. Withheld below 100 answers. |
| `mean` | The arithmetic mean of the answered round trips. One slow answer moves it, so read it beside the median. Withheld below 5 answers, marked below 20. |
| `min  max` | The extremes of the answered round trips. The maximum is the number most sensitive to this host being busy at the moment a reply arrived. |
| `sd` | The sample standard deviation, dividing by n - 1, of the answered round trips. It assumes nothing about their shape, and latency is usually skewed. Withheld below 20 answers. |
| `Δ` | RTT variation: the mean absolute difference between the round trips of two probes sent one after the other, both answered. A pair is dropped if anything lies between them — an unanswered probe, a skipped slot, a gap, a change of destination. Needs 10 pairs. It is a round-trip quantity sampled at the probe interval: not the one-way delay variation of RFC 3393 and not the jitter of RFC 3550. |
| `n` | The number of answered round trips behind every time shown for that stream and window. |

## Shares

Of the probes sent within the window. Shown as a count ("2 / 12") until there are enough probes for a percentage to mean something.

| Shown as | Meaning |
|---|---|
| `no-reply` | The share of finalised probes that got no answer: timed out or reported unreachable, over everything that was sent and has reached an outcome. A percentage from 20 finalised probes; below that, the count. It is not proof of packet loss on your line: the request may have been lost, or the reply, or the target may limit how often it answers, or the answer may have come after the deadline. |
| `err-resp` | The share of finalised probes that were answered, but not as expected: an error code, an answer the query's expectation does not allow, a blocked answer, or bytes that are not a valid answer to the question. An answer arrived, so none of these is a timeout. A percentage from 20 finalised probes; below that, the count. |
| `cov` | Coverage: the probes sent within the window over the number of probe intervals the window holds, at most 100 %. A young session, a gap, a stop, or skipped slots lower it. It says how much of the window the other numbers speak for. |

## Counts

Of the probe slots within the window. Every slot ends as exactly one of these.

| Shown as | Meaning |
|---|---|
| `sent` | Probes handed to the network: pending, replied, timed out, unreachable, error responses and cancelled together. |
| `pending` | Sent and still waiting: neither answered nor past the deadline. Not counted in any share. |
| `replied` | Answered before the deadline with what was expected. Only these have a round-trip time in the statistics. |
| `timed out` | Nothing came back before the deadline. A reply that arrives afterwards is noted as late and the probe stays timed out. |
| `unreachable` | An explicit refusal instead of an answer: a router's destination-unreachable message, a closed port, a refused connection. |
| `error responses` | Answered, but with an error code, an answer the expectation does not allow, or something that is not a valid answer. The numerator of err-resp. |
| `local errors` | The send failed on this host — no route, no address of that family, permission refused. Nothing reached the network, so these are never counted as unanswered. |
| `skipped` | A probe slot that was deliberately not used: the rate limit was reached, results were not being taken fast enough, or the timer fired too late for the slot to mean anything. Never counted as unanswered. |
| `cancelled` | Sent, and then the program could not vouch for the wait: the host slept, the network changed, collection was stopped, or the session ended. Not counted as unanswered. |
| `late` | Replies that arrived after the deadline, within the late window. Each is recorded with its time; the probe it belongs to remains timed out. |
| `duplicate` | Further replies to a probe that had its answer. The first one's time stands. |
| `unanswered run` | The longest run of consecutive probes with no answer in the window, and the run in progress. Any answer ends a run, an error response included; so does a gap. |

## Status words

One per stream, and one per card for its main stream. Carried by a symbol and a word, never by colour alone. The thresholds are the defaults; `[incidents]` in the configuration changes them.

| Shown as | Meaning |
|---|---|
| `OK` | The stream is answering and no rule below has opened. It is drawn in the ordinary text colour, not in green. |
| `WARMING UP` | Nothing has been sent yet, or fewer than 20 probes have been finalised this session and nothing is open against the stream. A stream that probes once a minute stays here for 20 minutes. |
| `NO REPLY` | 5 consecutive probes went unanswered (3 on a DNS stream). It ends after 10 consecutive answers (3 on a DNS stream). |
| `LOSSY` | 5 % or more of the probes of the last 60 s went unanswered, out of at least 30. It ends when the share has stayed at or below 1 % for 30 s. The word describes the stream's answers, not your line. |
| `SLOW` | The median of the last 60 s is more than 1.5 times the median of the 15 minutes before it, and at least 10 ms above it. It needs 30 recent answers and 300 in the baseline, so it is only ever said of a stream that has been running a while. The baseline is frozen while it is open; it ends when the median has stayed below 1.2 times the baseline for 60 s. |
| `ERRORS` | 3 of the last 10 DNS transactions on the stream were error responses. Answers arrived; these are not timeouts. It ends after 10 consecutive transactions answered as expected. |
| `LOCAL ERR` | Sending is failing on this host. Nothing reached the network, so nothing is said about the target and nothing is counted as unanswered. It ends with the first send that works. |
| `STALE` | The stream should be producing results and has not for three probe intervals (five seconds at least) plus one deadline. The numbers shown are old. |
| `NEEDS CONFIG` | The provider or target is waiting for a value only you have. Nothing is sent, and nothing is counted as unanswered. |
| `UNSUPPORTED` | This host does not permit the probe, or the target has no endpoint for this protocol. The reason is shown. Nothing is sent, and it is never reported as the target failing. |
| `DISABLED` | Switched off in the configuration, or left out by the mode or address family in use. |

## Marks

What stands in a cell instead of, or beside, a number, and what the outcome strip under a chart is made of.

| Shown as | Meaning |
|---|---|
| `—` | A dash in place of a number: there are too few samples for that statistic to be shown at all. In ASCII mode it is drawn as -. |
| `·` | A raised dot after a number: the number is shown, and rests on fewer than 20 samples. In ASCII mode it is drawn as a full stop. |
| `outcome strip` | The row of marks under a chart, one per column of time: · every probe answered, ▒ some unanswered, █ none answered, E an error response, ! a local error, ┄ skipped, ╌ a gap in observation, blank where nothing was due. In ASCII mode: . o # E ! _ ~. |
