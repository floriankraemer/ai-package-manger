# aip

aip is a package and skill manager for AI agent harnesses such as Claude Code, Codex, Cursor, Copilot, and Gemini CLI.
It works like Composer: you list the skills you need in `aip.json`, aip resolves versions and pins them in `aip.lock`, and `aip install` reproduces exactly that state on any machine.
Skills come from git repositories and are copied into the skill folder of each AI tool you use.

## Installation

aip is a single Rust binary.
It needs `git` on your `PATH` at run time.

Build from source with Rust 1.89 or newer:

```sh
git clone <repository-url> aip
cd aip
cargo build --release
```

Copy `target/release/aip` (`aip.exe` on Windows) to a folder on your `PATH`.

Without a local Rust toolchain, build with Docker Compose and `make`:

```sh
make dist    # writes ./dist/aip (Linux) and ./dist/aip.exe (Windows)
```

## Usage

Create a project configuration, then find and install a skill:

```sh
cd my-project
aip init --provider claude-code      # or just `aip init` to detect your providers
aip search pdf                       # find skills in the registry
aip info anthropics/skills/pdf               # read the full description of one skill
aip install anthropics/skills/pdf            # add it to aip.json and install it
```

```
$ aip install anthropics/skills/pdf
Package operations: 1 install, 0 updates, 0 removals
  - Installing anthropics/skills/pdf (1.2.0)
Skill folders: .claude/skills
```

Commit `aip.json` and `aip.lock`.
A teammate then runs `aip install` and gets the same skills at the same commits.

Keep skills current:

```sh
aip check                            # installed versions and available updates
aip update                           # move to the newest versions aip.json allows
aip remove anthropics/skills/pdf             # remove a skill and delete its folder
```

```
$ aip check
┌────────────┬───────────┬────────────┬────────┬─────────────────────────┐
│ Package    │ Installed │ Constraint │ Latest │ Status                  │
├────────────┼───────────┼────────────┼────────┼─────────────────────────┤
│ demo/hello │ 1.0.0     │ ^1.0       │ 2.0.0  │ update available: 1.1.0 │
└────────────┴───────────┴────────────┴────────┴─────────────────────────┘
Run `aip update` to apply 1 available update.
```

Pin versions with Composer-style constraints:

```sh
aip install anthropics/skills/pdf:^1.2 anthropics/skills/xlsx:dev-main
```

Other commands and options:

| Command | Purpose |
|---|---|
| `aip detect [PATH]` | Show which AI providers a folder is configured for |
| `aip init [--provider <id>]...` | Create `aip.json` |
| `aip search <query>` | Search the registry by name or description |
| `aip info <repository>/<skill>` | Show the full description of a skill |
| `aip check`, `install`, `update`, `remove` | Manage installed skills; add `-g/--global` to use your home folder, like `composer global` |

`search` and `info` also take `-g/--global` to use the global registries.
`aip check` exits with 1 on errors; `aip check --strict` exits with 1 unless every skill is up to date, for CI.

`--json` gives machine-readable output for `detect`, `search`, `info`, and `check`.

The [user guide](docs/user-guide.md) covers registries, version constraints, private repositories, and troubleshooting.

## Development

All tooling runs in Docker; you only need Docker Compose and `make`.

```sh
make          # list all targets
make check    # format check, clippy, tests
make run ARGS="detect --json ."
```

See the [documentation index](docs/README.md) for the architecture documents and decisions.

## Copyright

Copyright © 2026 Florian Krämer.

aip is free software, licensed under the GNU General Public License, version 3.
See the [LICENSE](LICENSE) file for the full text.
