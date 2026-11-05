---
name: symfony
description: Use when writing or reviewing Symfony framework code — controllers, Doctrine, DI, security component, forms/validation, and performance. Trigger on "write Symfony", "review this Symfony code", "Symfony best practice", "API Platform", or before/after implementing Symfony backend features. Pair with php for language-level conventions and security-audit-php-application for security-specific review.
---

# Symfony — Modern Best Practices

Target Symfony 6/7, PHP 8.3+. Assumes autowiring/autoconfiguration on (Symfony default since 4.0) — manual service wiring flagged unless there's a documented reason.

## Architecture

- Controllers thin: extract request data, call a service/use-case, return a response — no business logic, no Doctrine query building inline.
- Business logic in domain/application services, not entities-as-god-objects or controllers.
- DTOs at the request boundary (via `MapRequestPayload` or manual denormalization) + Symfony Validator constraints, not `$request->request->all()` fed straight into entities.
- Use Messenger for async work (email, external API calls, heavy processing) instead of blocking the request cycle.
- Value objects for domain concepts; avoid primitive obsession in entity properties.

## Dependency injection

- Constructor injection, autowired — no `ContainerInterface` injected into services to pull dependencies manually (service locator antipattern, hides real dependencies).
- Interfaces for services with more than one implementation or that need test doubles; skip the interface for services with exactly one implementation and no foreseeable second (YAGNI, don't pre-abstract).
- `#[Autoconfigure]`/tagged services for extension points (event subscribers, voters) rather than manual service.yaml wiring where attributes suffice.

## Doctrine

- Avoid N+1: explicit `JOIN` + `addSelect`, or `EAGER` fetch only where genuinely always-needed — don't blanket-eager-load everything.
- Use DQL/QueryBuilder for read queries shaped for a specific view instead of hydrating full entity graphs and filtering in PHP.
- Migrations (`doctrine:migrations`) for every schema change — never hand-edit the DB in an environment shared with others, never rely on `schema:update` outside local dev.
- Repository methods express intent (`findActiveByOrganization()`), not generic `findBy()` calls scattered through business logic.
- Watch for the identity map growing unbounded in long-running processes (Messenger workers, CLI batch jobs) — `EntityManager::clear()` periodically or use `iterate()`/batching for large result sets.

## Security component

- Voters for authorization logic beyond simple role checks — don't scatter `if ($user->getRole() === ...)` through controllers.
- `#[IsGranted]` attribute on controllers/methods over manual `denyAccessUnlessGranted()` calls where declarative suffices.
- Password hashing via the configured hasher (`UserPasswordHasherInterface`) — never roll custom hashing or use `md5`/`sha1`.
- CSRF protection enabled on all state-changing forms (default in Symfony Forms) — don't disable it without a documented reason (e.g. pure JSON API with token-based auth).
- Rate-limit login and other sensitive endpoints via the RateLimiter component.

## Performance

- OPcache enabled and preloaded in production (`opcache.preload`); Symfony cache warmed up as part of deploy (`cache:warmup`), not on first request.
- HTTP cache (`Cache-Control`, ESI, or a reverse proxy) for cacheable responses instead of re-computing on every request.
- Doctrine result cache for expensive, rarely-changing queries; query cache always on in prod.
- Autowiring/container compilation is a compile-time cost, not runtime — don't avoid DI "for performance," it's already compiled away.
- Profile with Blackfire/Xdebug before optimizing — don't guess at hot paths.

## Testing

- PHPUnit with Symfony's `WebTestCase`/`KernelTestCase` for integration tests against a real test database, not mocked Doctrine.
- Unit test domain/application services in isolation with mocked I/O boundaries (HTTP clients, mailers) only.

## What to avoid

- Fat controllers, fat entities — logic belongs in services.
- `$container->get()` / service locator pulls instead of constructor injection.
- Disabling CSRF or Doctrine's parameter binding (raw SQL string concatenation with user input — always bind parameters).
- `EAGER` fetch as a default fix for N+1 — it just moves the cost to every query instead of the ones that need it.
- Business logic in event listeners that silently mutate unrelated state — makes control flow hard to trace; prefer explicit service calls where behavior must be predictable.
