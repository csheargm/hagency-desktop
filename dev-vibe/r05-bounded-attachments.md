# R05: bounded attachment downloads

The Desktop audit found whole-response buffering in file downloads, previews and avatars. This change ports the owned transfer design from Client PR 21 while retaining Desktop's pinned official Matrix SDK. It depends on the credential/session lifecycle in Desktop PR 57; no SDK fork or dependency upgrade is included.

- Four original download workers, sixteen queued metadata requests, 64 KiB file copy/decryption buffers. Two owned consumer workers and a bounded queue keep filesystem work off UI/Tokio threads. Cancelling an observer does not release an operation's original file ownership.
- A shared 16 GiB temporary-file quota includes partial/ciphertext/plaintext/save staging and failed cleanup. Separate admission envelopes reserve three times the known response size before reading; unknown-length responses grow their envelope before each write. The reservation follows the original files until confirmed cleanup. Resource exhaustion fails explicitly instead of awaiting space while holding partial files.
- Ordinary file downloads have no separate 1 GiB cutoff. Previews are independent: images 8 MiB, themes 256 KiB, Markdown 512 KiB. Full Save As/Share uses the file-transfer policy regardless of the attachment's preview type.
- Reuses the SDK HTTP/TLS client, negotiated authenticated/legacy Ruma endpoints, token refresh and the SDK attachment decryptor. Plaintext is published only after complete ciphertext SHA-256 verification and fsync. Account epochs/client identity fence late work.
- Save As copies from the verified file into private destination staging and atomically publishes only while its session is current. Cleanup owns original physical file identities; it never deletes the published user destination.
- Remote media can be fetched again after eviction with its existing Matrix attachment descriptor and room keys. No crypto database, room key, event or descriptor is deleted by this transport.

## Official SDK cache compatibility

The pinned SDK exposes only whole-row media reads. Before a local queued-upload cache read, the reader obtains the SDK cross-process media lock and checks the SQLite database's reported logical size as a conservative upper bound. Unknown/oversized stores are never read unbounded. Remote previews bypass such a cache and use bounded HTTP; a pending local echo reports that it must wait for upload completion. This restriction is deliberate and visible: it is not a claim that the SDK now supports bounded row reads. Tests exercise an encrypted official SQLite store and prove read rejection does not delete its queued content. A future row-bounded SDK API can replace this conservative adapter without changing the transfer protocol.

R07 separately replaces the preexisting native share staging lifecycle. This PR's streamed share path removes full payload buffering but does not claim to close R07.

## Validation

Real localhost HTTP tests cover authenticated and legacy paths, actual chunk counts without Content-Length, SHA-256 failure/truncation, refresh and retry policy, queue saturation, cancellation, retirement, cleanup failure/identity replacement and atomic-save recovery. A real file larger than 1 GiB is downloaded, every byte checked through a bounded writer, saved and preserved after temporary cleanup.

The regression runner now includes transfer, preview and SDK-cache tests alongside the existing Markdown/activity/copy/forward/export gates. Detailed command results are recorded in the PR description. Live account, network failure after homeserver deletion, Linux/Windows native dialogs and OS-wide RSS are not certified by the localhost fixtures.
