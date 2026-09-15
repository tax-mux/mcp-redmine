# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-09-15

### Added

- Multi-profile API keys via `REDMINE_API_KEYS` / `REDMINE_API_KEYS_FILE`
- Identity via `X-Redmine-Profile` (tool-arg `profile` principally forbidden)
- Issue create/update ACK, attachment upload/delete, argument normalization
- Optional known-project fallback from `REDMINE_KNOWN_PROJECTS_FILE`
- Public-oriented README and `examples/known-projects.json`

### Changed

- Projects list response thinned to id/name/identifier + total_count
- Default profile fallback disabled (missing/`default` header rejected)

### Security

- API keys stay in-container; stripped/redacted from MCP responses and errors
