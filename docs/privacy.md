# Rinx data and privacy

Rinx connects to the Matrix homeserver you choose. That server's policies govern
account and message data it receives. Authentication happens through the app or
your server's browser sign-in provider.

The app stores session information, room caches, settings, article drafts, and
selected article assets in its local profile. Rinx uses its own default profile;
`HAGENCY_DESKTOP_DATA_DIR` can select a different absolute directory.

Mini-app authorization is tied to the signed-in Matrix user and the requested
capabilities. The article mini app receives scoped host operations, not the
account password or a raw Matrix access token. Publishing or sharing an article
sends its content to the selected Matrix conversation.

Native HTML/CSS article preview uses the app's authorized article assets. Other
web mini apps and links you choose to open may contact their own sites. Optional
Hagency integrations communicate with the services configured for that workflow.

Logs and exported diagnostics can contain account or room identifiers. Review
them before sharing them in a public issue. Technical details of mini-app grants
are documented in [ADR 0002](adr/0002-octoscript-mini-app-authority.md).
