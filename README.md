# PLOY has moved to Monday

PLOY is now maintained as the independent product workspace at
[`proerror77/monday/products/ploy`](https://github.com/proerror77/monday/tree/main/products/ploy).

The migration was merged in
[`proerror77/monday#8`](https://github.com/proerror77/monday/pull/8) at Monday merge
commit `c69c9b6b2252eec57de48f2a281642f01e460d12`.

This former standalone repository is no longer maintained. Its issues, pull
requests, releases, Actions history, and Git history remain available as historical
evidence, but its workflows, deployment instructions, hosts, environments, and
execution paths are not current Monday authority.

For active development, bug reports, and architecture changes, use the
[`proerror77/monday`](https://github.com/proerror77/monday) repository. Monday's
`rust_hft` workspace is the sole production authority for risk, OMS,
reconciliation, cancellation, and order execution; live trading remains disabled
for the imported PLOY workspace unless a separate reviewed Monday change restores
that path.

## Preserved source baseline

- Former PLOY `main` snapshot: `8ce4e0f150173a44030294101f4b1371cbdf80bc`
- Monday location: `products/ploy`
- Migration provenance: [`MIGRATION_ADAPTATIONS.md`](https://github.com/proerror77/monday/blob/main/products/ploy/MIGRATION_ADAPTATIONS.md)
