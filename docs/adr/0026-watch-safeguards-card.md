# 0026. A Safeguards card for each issue watch

Status: accepted (Chris, 2026-11-17: loop safeguards queue, "Safeguards card per watch")

## Context
An owner who turns on issue watching, push freely and agent merges has no
single place that says what the Agent may now do without asking. The facts
are spread over the issue loop (ADR 0020: attempts, free retries, hand-off
timeout), push freely (ADR 0022: which git and gh steps skip the card, never
`main`/`master` or the default branch), and the merge gate
(the merge rules, protected paths, the per-Agent "Pull request
merges" switch). Before the week-long loop measurement the owner needs to
see them at one glance, the way Buzz shows an agent's identity in one line.

## Decision
Under the "Issue watching" block of the Agent panel's Settings, a
**Safeguards** card (`src/hagency/issue_watch/safeguards_card.rs`) shows,
while the watch is on and this device runs the Agent:

- **May do alone**: the push-freely steps when that switch is on, and
  "approve and merge pull requests the merge rule allows" only when the
  Agent's merge switch is on AND the repository's policy file is readable.
- **Needs you**: everything outside push freely, deleting or force-pushing
  branches, and merges (every merge when the switch is off, the file is
  missing or the gate refuses it).
- **Limits**: tries per issue, free retries, minutes per try, the merge rule
  as the gate parses it (paths, line limit, CI, approvals with the gate's
  floor), and the paths that always need a person (protected prefixes,
  `CODEOWNERS`, the policy's `always_human`).
- One line recommending GitHub branch protection on the default branch,
  saying whether it is on when GitHub's branch reply (`protected`) can be read.

### Every line comes from the enforced value (R3)
The card never repeats a number or name. The engine's constants are
re-exported from `hagency::issue_watch::safeguards` (`MAX_ATTEMPTS`,
`MAX_FREE_RETRIES`, `HANDOFF_TIMEOUT_SECS`, `POLICY_PATH`,
`PROTECTED_PREFIXES`, `CODEOWNERS`, `MIN_AGENT_APPROVALS`, push freely's
protected branches), which only needed `pub` on existing constants. The policy
file is parsed by the gate's own `parse_policy`, so a rule the gate refuses
reads as "not valid" on the card too. The pure model
(`issue_watch/safeguards.rs`) takes an `Enforced` value built from those
constants; tests change a field and see the line change.

### Read path
One read-only GitHub pass per card load with the Agent's token: the
repository (default branch), the policy file on the default branch
(contents API, 404 = no file), and the branch's `protected` flag (a failed
read is "unknown", never "off"). The gate reads the policy at each pull
request's base commit; the card reads the default branch tip, which is the
base of the Agent's own pull requests. The card reloads when the issue-watch
block saves or re-reads, or the merge switch changes.

### Trial mode
The parallel trial-mode setting ("first N pull requests need the owner's
merge") is read through one accessor, `safeguards::trial_owner_merges`, from
the saved watch's `trialOwnerMerges`; absent means no trial line. The
trial-mode change wires its real field into that accessor.

## Consequences
- Plain-language text in English and Chinese (`resources/i18n`).
- A wording change needs no code change elsewhere; a limit change needs no
  card change.
- The work-list repositories (ADR 0016) do not get a card yet; their merge
  rules differ (owner rules) and would need their own lines.

Update (ADR 0029): the "Merge rule" line reads the watch room's merge rules
("Merge rule (room policy): …"), not a repository file; with none it names
where a room admin sets them and the suggested low-risk paths.
