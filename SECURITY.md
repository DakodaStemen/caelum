# Security policy

## Supported versions

Security fixes are made for the latest release (or `main`, if no release has been tagged) only.

## Reporting a vulnerability

Do not open a public issue for security problems. Use GitHub's private vulnerability reporting
("Report a vulnerability" on the Security tab of github.com/DakodaStemen/caelum) if it is enabled;
otherwise contact the maintainer, [@DakodaStemen](https://github.com/DakodaStemen), directly.
Include the affected version or commit, a description, and steps to reproduce.

Caelum runs shells and reads and writes user files, so issues in terminal escape-sequence handling, PTY or process handling, recovery and workspace files, and anything that could lose or overwrite user data are in scope.

## What not to commit

- Secrets: API keys, tokens, passwords, private keys, `.env` files.
- Personal data of any kind.

If you commit one by mistake, tell the maintainer straight away and rotate the credential first;
a new commit alone does not remove it from history.
