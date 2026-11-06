# aip documentation

## For users

- [User guide](user-guide.md): install, concepts, commands, version constraints, publishing skills, registries, troubleshooting.

## For contributors

### arc42 architecture documentation

| Section | File |
|---|---|
| 01 Introduction and goals | [01-introduction-and-goals.md](architecture/arc42/01-introduction-and-goals.md) |
| 02 Constraints | [02-constraints.md](architecture/arc42/02-constraints.md) |
| 03 Context and scope | [03-context-and-scope.md](architecture/arc42/03-context-and-scope.md) |
| 04 Solution strategy | [04-solution-strategy.md](architecture/arc42/04-solution-strategy.md) |
| 05 Building block view (includes the registry abstraction) | [05-building-block-view.md](architecture/arc42/05-building-block-view.md) |
| 06 Runtime view | [06-runtime-view.md](architecture/arc42/06-runtime-view.md) |
| 07 Deployment view | [07-deployment-view.md](architecture/arc42/07-deployment-view.md) |
| 08 Cross-cutting concepts | [08-crosscutting-concepts.md](architecture/arc42/08-crosscutting-concepts.md) |
| 09 Architecture decisions | [09-architecture-decisions.md](architecture/arc42/09-architecture-decisions.md) |
| 10 Quality requirements | [10-quality-requirements.md](architecture/arc42/10-quality-requirements.md) |
| 11 Risks and technical debt | [11-risks-and-technical-debt.md](architecture/arc42/11-risks-and-technical-debt.md) |
| 12 Glossary | [12-glossary.md](architecture/arc42/12-glossary.md) |

### Architecture decision records

- [0001 Rust over Go](architecture/decisions/0001-rust-over-go.md)
- [0002 Composer model](architecture/decisions/0002-composer-model.md)
- [0003 git CLI instead of libgit2](architecture/decisions/0003-git-cli-instead-of-libgit2.md)
- [0004 Versioning by git tags](architecture/decisions/0004-versioning-by-git-tags.md)
- [0005 Pluggable registries](architecture/decisions/0005-pluggable-registries.md)
- [0006 Docker Compose dev environment](architecture/decisions/0006-docker-compose-dev-environment.md)
- [0007 Authentication delegated to git](architecture/decisions/0007-authentication-delegated-to-git.md)
- [0008 HTTP client: ureq with rustls](architecture/decisions/0008-http-client-ureq.md)
- [0009 auth.json for HTTP registries](architecture/decisions/0009-auth-json-for-http-registries.md)
- [0010 Repository names from URLs](architecture/decisions/0010-repository-names-from-urls.md)

The C4 views are embedded in the arc42 sections: context in 03, containers and components in 05.
