# Current device setup checkpoint

The built-in Hagency app accepts the current device states `uninitialized`,
`active`, and `unavailable`. An uninitialized account opens a native device
setup review. The native host verifies the current Matrix owner and device and
creates the current profile. This step creates no provider account or shared
quota. An active account continues to provider accounts and resources.

`hagency.onboarding.status` and `hagency.onboarding.review` accept only `{}`.
The script cannot supply identity, profile paths, resource imports, a monthly
account limit, or setup confirmation. Closing before confirmation has no effect.
An interrupted current setup reopens its saved initialization identity.

Server admission and publication remain separate actions. Saved publication
reviews use the original `intentId`, with owner, executor, account and current
period authorization rechecked by the native host. Viewing resource inventory
does not authorize publishing capacity. Current grant closure retains actual
committed amounts and unresolved work.

`check_onboarding.py` runs the shipped helpers and view in the pinned VM in
English and Chinese. It covers current setup, native close/unknown outcomes,
current account navigation, original publication handoff, invalid status
rejection, busy/authority refusals and preservation of an unrelated saved command.
`check_owner_tasks.py` covers the current device hub and actual pool/account
selectors. These checks exercise callbacks and view construction, not pixels or
live Matrix/provider effects.
