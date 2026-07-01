# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for security problems.

Instead, report privately via GitHub's
[Security Advisories](https://github.com/Rocho-EL-Locho/mcp-manager/security/advisories/new)
(Security → Report a vulnerability), or contact the maintainer directly.

Please include steps to reproduce and the affected version/commit. You'll get an
acknowledgement as soon as possible.

## Scope notes

mcp-manager is a local desktop app that manages Claude Code's MCP server
configuration. It reads and writes files under your home directory
(`~/.claude.json`, `~/.claude/settings*.json`, project `.mcp.json`) and shells
out to the official `claude` CLI — always with argument vectors, never a shell
string. Server secrets (env values, headers, inline tokens in args) are masked
in the UI by default and only revealed on an explicit user action; they are
never transmitted anywhere. Never commit real secrets to the repository.
