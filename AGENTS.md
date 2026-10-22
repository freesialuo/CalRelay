# Development rules

- Start from a tracked GitHub Issue. Use `<type>/<issue>-<slug>` branches and PRs targeting `main`.
- Read `CONTRIBUTING.md` and the relevant architecture documents before changing behavior.
- Keep changes within the Issue's acceptance criteria; update contracts and documentation when behavior changes.
- For complex implementations, use Context7 or Exa to consult official documentation and public code; retain the references actually used.
- Select testing, proof and quality tools by Issue risk and TDD needs using [the verification toolkit](docs/verification-toolkit.md); listed capabilities do not imply installation or mandatory CI.
- Run applicable checks before requesting review. Do not represent skipped or unavailable tests as passing.
- Do not push directly to `main`, force-push shared history, self-approve PRs, bypass repository rules, or enable automatic merging.
- This is a solo-maintainer project: do not require independent reviewers or GitHub approving reviews. Merge only after required CI, resolved discussions, review of the final diff, and explicit maintainer authorization for that PR. Changing the reviewed diff requires renewed authorization.
- Use `find … -delete` to delete files; do not use `rm`.
- Prefer simple implementations and concise communication. Never commit credentials, runtime databases or local secret files.
- Do not spawn sub-agents unless explicitly requested by the user.

The first repository baseline is a one-time initialization exception to the PR requirement. It contains only `.gitattributes`; all architecture and development tooling must enter through the initial PR.
